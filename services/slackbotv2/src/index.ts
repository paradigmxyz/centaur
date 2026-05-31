import { Hono } from 'hono'
import { Chat, type Attachment, type Logger, type Message, type StateAdapter, type Thread } from 'chat'
import { createSlackAdapter } from '@chat-adapter/slack'
import { createPostgresState } from '@chat-adapter/state-pg'
import {
  codexAppServerToChatSdkStream,
  type CodexAppServerToChatStreamOptions,
  type ServerNotification,
  type Turn
} from '@centaur/harness-events'

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

const noopLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => noopLogger
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
  const state = (await thread.state) ?? {}
  const messageIds = new Set(state.forwardedMessageIds ?? [])
  const isDuplicateIncrementalMessage =
    messageIds.has(message.id) && (!input.includeContext || state.historyForwarded)
  if (isDuplicateIncrementalMessage) return

  const serializedMessage = await serializeMessage(message)
  let context: SlackbotV2ApiMessage[] | undefined

  if (input.includeContext && !state.historyForwarded) {
    context = await collectInitialContext(thread, message)
    for (const item of context) {
      messageIds.add(item.id)
    }
  } else {
    messageIds.add(serializedMessage.id)
  }

  const shouldOpenStream = input.execute && state.activeExecution !== true
  let lastEventId = state.lastEventId ?? 0
  const stream = await forwardToSessionApi(input.options, {
    afterEventId: lastEventId,
    executeMessage: input.execute ? serializedMessage : undefined,
    messages: context ?? [serializedMessage],
    onEventId: eventId => {
      lastEventId = Math.max(lastEventId, eventId)
    },
    openStream: shouldOpenStream,
    threadId: thread.id
  })

  await thread.setState({
    activeExecution: state.activeExecution || shouldOpenStream,
    forwardedMessageIds: Array.from(messageIds).slice(-1000),
    historyForwarded: state.historyForwarded || input.includeContext,
    lastEventId: state.lastEventId
  })

  if (!stream) return

  try {
    await renderExecutionStream(thread, stream, serializedMessage, input.options)
  } finally {
    const latest = (await thread.state) ?? {}
    await thread.setState({
      ...latest,
      activeExecution: false,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId)
    })
  }
}

async function renderExecutionStream(
  thread: Thread,
  stream: AsyncIterable<ServerNotification>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options
): Promise<void> {
  await setAssistantTitle(thread, titleFromMessage(message.text, options.userName))
  await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...')
  try {
    await thread.post(
      codexAppServerToChatSdkStream(
        withAssistantTitleUpdates(thread, stream),
        options.mapper
      )
    )
  } finally {
    await setAssistantStatus(thread, '')
  }
}

async function* withAssistantTitleUpdates(
  thread: Thread,
  stream: AsyncIterable<ServerNotification>
): AsyncIterable<ServerNotification> {
  for await (const notification of stream) {
    if (notification.method === 'thread/name/updated') {
      await setAssistantTitle(thread, notification.params.threadName)
    }
    yield notification
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
  input: {
    afterEventId: number
    executeMessage?: SlackbotV2ApiMessage
    messages: SlackbotV2ApiMessage[]
    onEventId(eventId: number): void
    openStream: boolean
    threadId: string
  }
): Promise<AsyncIterable<ServerNotification> | null> {
  await appendSessionMessages(options, input.threadId, input.messages)
  if (!input.executeMessage) return null

  await executeSession(options, input.threadId, input.executeMessage)
  if (!input.openStream) return null

  return streamSessionNotifications(options, input.threadId, input.afterEventId, input.onEventId)
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
): Promise<AsyncIterable<ServerNotification>> {
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

function apiSessionUrl(apiUrl: string, threadId: string, suffix?: 'messages' | 'execute' | 'events'): string {
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
): AsyncIterable<ServerNotification> {
  for await (const event of parseSseEvents(stream)) {
    if (typeof event.id === 'number') onEventId(event.id)
    if (event.event === 'session.output.line') {
      const output = notificationFromCodexOutputLine(event.data)
      if (output.notification) yield output.notification
      if (output.terminal) return
      continue
    }
    if (event.event === 'session.execution_failed' || event.event === 'session.stream_error') {
      yield sessionErrorNotification(event)
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

function notificationFromCodexOutputLine(line: string): {
  notification?: ServerNotification
  terminal: boolean
} {
  let payload: unknown
  try {
    payload = JSON.parse(line)
  } catch {
    return {
      notification: {
        method: 'error',
        params: { error: { message: `invalid Codex output line: ${line}` } }
      } as ServerNotification,
      terminal: true
    }
  }

  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) {
    return { terminal: false }
  }

  const object = payload as Record<string, unknown>
  if (typeof object.method === 'string') {
    return {
      notification: object as unknown as ServerNotification,
      terminal: isTerminalCodexPayload(object)
    }
  }

  const type = stringValue(object.type)
  if (!type) return { terminal: false }
  return {
    notification: dotTypePayloadToNotification(type, object),
    terminal: isTerminalCodexPayload(object)
  }
}

function dotTypePayloadToNotification(type: string, payload: Record<string, unknown>): ServerNotification {
  const params = { ...payload }
  delete params.type

  if (type === 'thread.name.updated') {
    return {
      method: 'thread/name/updated',
      params: {
        threadId: stringValue(params.threadId) ?? stringValue(params.thread_id) ?? '',
        threadName:
          stringValue(params.threadName) ??
          stringValue(params.thread_name) ??
          stringValue(params.name) ??
          'Centaur task'
      }
    } as ServerNotification
  }

  if (type === 'turn.started') {
    const turnId = stringValue(params.turnId) ?? stringValue(params.turn_id) ?? 'turn'
    return {
      method: 'turn/started',
      params: {
        threadId: stringValue(params.threadId) ?? stringValue(params.thread_id) ?? '',
        turn: emptyTurn(turnId, 'inProgress')
      }
    } as ServerNotification
  }

  if (type === 'turn.completed') {
    return {
      method: 'turn/completed',
      params: {
        threadId: stringValue(params.threadId) ?? stringValue(params.thread_id) ?? '',
        turn: normalizeTurn(params.turn, 'completed')
      }
    } as ServerNotification
  }

  if (type === 'turn.failed' || type === 'error') {
    const error = params.error
    const message =
      typeof error === 'string'
        ? error
        : error && typeof error === 'object' && 'message' in error
          ? String((error as { message?: unknown }).message ?? 'Codex turn failed')
          : stringValue(params.message) ?? 'Codex turn failed'
    return {
      method: 'error',
      params: { error: { message } }
    } as ServerNotification
  }

  const method =
    type === 'item.reasoning.summaryPartAdded'
      ? 'item/reasoning/summaryTextDelta'
      : type.replace(/\./g, '/')
  return { method, params } as unknown as ServerNotification
}

function normalizeTurn(value: unknown, status: 'completed' | 'inProgress'): Turn {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return {
      ...emptyTurn(stringValue((value as Record<string, unknown>).id) ?? 'turn', status),
      ...(value as Record<string, unknown>)
    } as Turn
  }
  return emptyTurn('turn', status)
}

function emptyTurn(id: string, status: 'completed' | 'inProgress'): Turn {
  return {
    id,
    items: [],
    itemsView: 'full',
    status,
    error: null,
    startedAt: null,
    completedAt: status === 'completed' ? Date.now() : null,
    durationMs: null
  }
}

function isTerminalCodexPayload(payload: Record<string, unknown>): boolean {
  return payload.type === 'turn.completed' || payload.type === 'turn.failed' || payload.method === 'error'
}

function sessionErrorNotification(event: ParsedSessionEvent): ServerNotification {
  let message = `${event.event ?? 'session error'}`
  try {
    const payload = JSON.parse(event.data) as Record<string, unknown>
    message = stringValue(payload.error) ?? stringValue(payload.message) ?? message
  } catch {
    if (event.data.trim()) message = event.data.trim()
  }
  return {
    method: 'error',
    params: { error: { message } }
  } as ServerNotification
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
