import type { RustSessionStreamEvent } from '@centaur/harness-events'
import type { Attachment, Message } from 'chat'
import type {
  ForwardSessionInput,
  JsonObject,
  JsonValue,
  SlackbotV2ApiAttachment,
  SlackbotV2ApiMessage,
  SlackbotV2AppendMessagesRequest,
  SlackbotV2CreateSessionRequest,
  SlackbotV2ExecuteSessionRequest,
  SlackbotV2ExecuteSessionResponse,
  SlackbotV2Options,
  SlackbotV2RendererSource,
  SlackbotV2SessionMessage
} from './types'
import { elapsedMs, isJsonObject, nowMs, stringValue, toAsyncIterable, traceLog } from './utils'

export class SessionApiError extends Error {
  readonly action: string
  readonly body: string
  readonly retryable: boolean
  readonly status: number
  readonly statusText: string

  constructor(input: {
    action: string
    body: string
    retryable: boolean
    status: number
    statusText: string
  }) {
    const suffix = input.body ? `: ${input.body}` : ''
    super(
      `Centaur session ${input.action} failed: ${input.status} ${input.statusText}${suffix}`
    )
    this.name = 'SessionApiError'
    this.action = input.action
    this.body = input.body
    this.retryable = input.retryable
    this.status = input.status
    this.statusText = input.statusText
  }
}

export function isRetryableSessionApiError(error: unknown): boolean {
  if (error instanceof SessionApiError) return error.retryable
  if (!(error instanceof Error)) return false
  return error.name === 'AbortError' || error.name === 'TypeError'
}

type ForwardSessionApiCallbacks = {
  onExecutionStarted?(execution: SlackbotV2ExecuteSessionResponse): Promise<void>
  onMessagesAppended?(): Promise<void>
}

export async function collectInitialContext(
  thread: { allMessages: AsyncIterable<Message> },
  currentMessage: Message
): Promise<SlackbotV2ApiMessage[]> {
  const messages: Message[] = []
  try {
    for await (const message of thread.allMessages) {
      messages.push(message)
    }
  } catch (error) {
    if (!isSlackThreadNotFoundError(error)) throw error
    return [await serializeMessage(currentMessage)]
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

function isSlackThreadNotFoundError(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false

  const directError = (error as { error?: unknown }).error
  if (directError === 'thread_not_found') return true

  const data = (error as { data?: unknown }).data
  if (isJsonObject(data) && data.error === 'thread_not_found') return true

  return error instanceof Error && error.message.includes('thread_not_found')
}

export async function serializeMessage(message: Message): Promise<SlackbotV2ApiMessage> {
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
    teamId: slackTeamId(message.raw) as string,
    text: message.text,
    threadId: message.threadId,
    timestamp: message.metadata.dateSent.toISOString()
  }
}

function slackTeamId(raw: unknown): string | undefined {
  if (!isJsonObject(raw)) return undefined
  const team = raw.team
  if (typeof raw.team_id === 'string' && raw.team_id) return raw.team_id
  if (typeof team === 'string' && team) return team
  if (isJsonObject(team) && typeof team.id === 'string' && team.id) return team.id
  const user = raw.user
  if (isJsonObject(user) && typeof user.team_id === 'string' && user.team_id) {
    return user.team_id
  }
  return undefined
}

export async function forwardToSessionApi(
  options: SlackbotV2Options,
  input: ForwardSessionInput,
  callbacks: ForwardSessionApiCallbacks = {}
): Promise<AsyncIterable<SlackbotV2RendererSource> | null> {
  const createStartedAtMs = nowMs()
  await createSession(options, input.threadId, input.harnessType)
  traceLog(options, 'slackbotv2_session_create_complete', input.trace, {
    phase_ms: elapsedMs(createStartedAtMs)
  })
  if (input.messages.length > 0) {
    const appendStartedAtMs = nowMs()
    await appendSessionMessages(options, input.threadId, input.messages)
    traceLog(options, 'slackbotv2_session_append_complete', input.trace, {
      message_count: input.messages.length,
      phase_ms: elapsedMs(appendStartedAtMs)
    })
    await callbacks.onMessagesAppended?.()
  } else {
    traceLog(options, 'slackbotv2_session_append_skipped', input.trace, {
      message_count: 0
    })
  }
  if (!input.executeMessage) return null

  const executeStartedAtMs = nowMs()
  const execution = await executeSession(
    options,
    input.threadId,
    input.executeMessage,
    input.model,
    input.executeContextMessages
  )
  traceLog(options, 'slackbotv2_session_execute_complete', input.trace, {
    execution_id: execution.execution_id,
    phase_ms: elapsedMs(executeStartedAtMs)
  })
  await callbacks.onExecutionStarted?.(execution)
  if (!input.openStream) return null

  return openSessionEventStream(options, input)
}

export async function openSessionEventStream(
  options: SlackbotV2Options,
  input: Pick<ForwardSessionInput, 'afterEventId' | 'executionId' | 'onEventId' | 'threadId' | 'trace'>
): Promise<AsyncIterable<SlackbotV2RendererSource>> {
  const streamStartedAtMs = nowMs()
  const stream = await streamSessionNotifications(
    options,
    input.threadId,
    input.afterEventId,
    input.executionId,
    input.onEventId
  )
  traceLog(options, 'slackbotv2_session_events_opened', input.trace, {
    after_event_id: input.afterEventId,
    execution_id: input.executionId,
    phase_ms: elapsedMs(streamStartedAtMs)
  })
  return stream
}

export function sessionStreamError(error: unknown): RustSessionStreamEvent {
  return {
    data: { error: error instanceof Error ? error.message : String(error) },
    event: 'session.stream_error',
    eventKind: 'session.stream_error'
  }
}

/** Largest attachment we are willing to buffer in memory and inline as base64. */
export const MAX_INLINE_ATTACHMENT_BYTES = 100 * 1024 * 1024
const MAX_CODEX_INPUT_LINE_CHARS = 900 * 1024
const STAGED_ATTACHMENT_CHUNK_CHARS = 700 * 1024

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

  if (typeof attachment.size === 'number' && attachment.size > MAX_INLINE_ATTACHMENT_BYTES) {
    serialized.fetchError = attachmentTooLargeError(attachment.size)
    return serialized
  }

  try {
    const data = attachment.data ?? (await attachment.fetchData?.())
    if (data) {
      // Re-check the actual byte count: Slack size metadata can be absent.
      const byteLength = Buffer.isBuffer(data) ? data.length : data.size
      if (byteLength > MAX_INLINE_ATTACHMENT_BYTES) {
        serialized.fetchError = attachmentTooLargeError(byteLength)
        return serialized
      }
      serialized.dataBase64 = await bytesToBase64(data)
    }
  } catch (error) {
    serialized.fetchError = error instanceof Error ? error.message : String(error)
  }

  return serialized
}

function attachmentTooLargeError(bytes: number): string {
  return `attachment too large to inline (${bytes} bytes > ${MAX_INLINE_ATTACHMENT_BYTES} byte limit)`
}

async function bytesToBase64(data: Buffer | Blob): Promise<string> {
  if (Buffer.isBuffer(data)) return data.toString('base64')
  const bytes = await data.arrayBuffer()
  return Buffer.from(bytes).toString('base64')
}

const DEFAULT_HARNESS_TYPE = 'codex'

async function createSession(
  options: SlackbotV2Options,
  threadId: string,
  harnessType?: string
): Promise<void> {
  const requested = harnessType ?? DEFAULT_HARNESS_TYPE
  const response = await postCreateSession(options, threadId, requested)
  if (response.ok) return

  let body = ''
  try {
    body = await response.text()
  } catch {
    body = ''
  }
  // A thread is pinned to the harness it was created with; the API rejects a
  // differing harness_type with 409. A mid-thread --claude/--amp/--codex (or a
  // plain message on a thread created with a non-default harness) lands here:
  // keep the thread alive on its existing harness instead of failing the message.
  const existing = response.status === 409 ? existingHarnessFromConflict(body) : undefined
  if (existing && existing !== requested) {
    const retry = await postCreateSession(options, threadId, existing)
    await ensureApiOk(retry, 'create session')
    return
  }
  throw new SessionApiError({
    action: 'create session',
    body,
    retryable: isRetryableApiStatus(response.status),
    status: response.status,
    statusText: response.statusText
  })
}

async function postCreateSession(
  options: SlackbotV2Options,
  threadId: string,
  harnessType: string
): Promise<Response> {
  const fetchFn = options.fetch ?? fetch
  const body: SlackbotV2CreateSessionRequest = {
    harness_type: harnessType,
    metadata: {
      source: 'slackbotv2',
      platform: 'slack',
      thread_id: threadId
    }
  }
  return fetchFn(apiSessionUrl(options.apiUrl, threadId), {
    method: 'POST',
    headers: apiHeaders(options),
    body: JSON.stringify(body)
  })
}

function existingHarnessFromConflict(body: string): string | undefined {
  try {
    const payload = JSON.parse(body)
    if (isJsonObject(payload)) {
      const existing = stringValue(payload.existing_harness)
      if (existing) return existing
    }
  } catch {
    // fall through to message parsing
  }
  return /already exists with harness_type ([A-Za-z0-9_-]+)/.exec(body)?.[1]
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
  message: SlackbotV2ApiMessage,
  model?: string,
  contextMessages?: SlackbotV2ApiMessage[]
): Promise<SlackbotV2ExecuteSessionResponse> {
  const fetchFn = options.fetch ?? fetch
  const body: SlackbotV2ExecuteSessionRequest = {
    idempotency_key: message.id,
    metadata: sessionMetadata(message, { action: 'execute' }),
    input_lines: toCodexInputLines(message, threadId, model, contextMessages),
    ...(options.idleTimeoutMs === undefined ? {} : { idle_timeout_ms: options.idleTimeoutMs }),
    ...(options.maxDurationMs === undefined ? {} : { max_duration_ms: options.maxDurationMs })
  }
  const response = await fetchFn(apiSessionUrl(options.apiUrl, threadId, 'execute'), {
    method: 'POST',
    headers: apiHeaders(options),
    body: JSON.stringify(body)
  })
  await ensureApiOk(response, 'execute session')
  return (await response.json()) as SlackbotV2ExecuteSessionResponse
}

async function ensureApiOk(response: Response, action: string): Promise<void> {
  if (response.ok) return
  let body = ''
  try {
    body = await response.text()
  } catch {
    body = ''
  }
  throw new SessionApiError({
    action,
    body,
    retryable: isRetryableApiStatus(response.status),
    status: response.status,
    statusText: response.statusText
  })
}

function isRetryableApiStatus(status: number): boolean {
  return status === 408 || status === 425 || status === 429 || status >= 500
}

async function streamSessionNotifications(
  options: SlackbotV2Options,
  threadId: string,
  afterEventId: number,
  executionId: string | undefined,
  onEventId: (eventId: number) => void
): Promise<AsyncIterable<SlackbotV2RendererSource>> {
  const fetchFn = options.fetch ?? fetch
  const url = new URL(apiSessionUrl(options.apiUrl, threadId, 'events'))
  url.searchParams.set('after_event_id', String(afterEventId))
  if (executionId) url.searchParams.set('execution_id', executionId)
  const response = await fetchFn(
    url.toString(),
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
    client_message_id: message.id,
    role: message.author.isMe ? 'assistant' : 'user',
    parts: sessionMessageParts(message),
    metadata: sessionMetadata(message)
  }
}

function sessionMessageParts(message: SlackbotV2ApiMessage): JsonValue[] {
  const parts: JsonValue[] = []
  if (message.text.trim()) {
    parts.push({ type: 'text', text: message.text })
  }
  for (const attachment of message.attachments) {
    parts.push(sessionAttachmentPart(attachment))
  }
  return parts.length > 0 ? parts : [{ type: 'text', text: '' }]
}

function sessionAttachmentPart(attachment: SlackbotV2ApiAttachment): JsonObject {
  const part: JsonObject = { ...attachment, attachment_type: attachment.type, type: 'attachment' }
  if (
    typeof attachment.dataBase64 === 'string'
    && attachment.dataBase64.length > MAX_CODEX_INPUT_LINE_CHARS
  ) {
    delete part.dataBase64
    part.dataBase64Omitted = `${attachment.dataBase64.length} base64 chars omitted from stored session message`
  }
  return part
}

function sessionMetadata(
  message: SlackbotV2ApiMessage,
  extra: JsonObject = {}
): JsonObject {
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

function toCodexInputLines(
  message: SlackbotV2ApiMessage,
  threadId: string,
  model?: string,
  contextMessages?: SlackbotV2ApiMessage[]
): string[] {
  const staged = new Map<SlackbotV2ApiAttachment, string>()
  const lines: string[] = []
  for (const attachment of message.attachments) {
    if (!attachment.dataBase64) continue
    const inlineLine = toCodexInputLineWithStaged(
      message,
      threadId,
      staged,
      model,
      contextMessages
    )
    if (
      inlineLine.length <= MAX_CODEX_INPUT_LINE_CHARS
      && attachment.dataBase64.length <= MAX_CODEX_INPUT_LINE_CHARS
    ) {
      continue
    }
    const stagedAttachmentId = `att-${message.id}-${staged.size + 1}`
    staged.set(attachment, stagedAttachmentId)
    lines.push(...stagedAttachmentInputLines(attachment, stagedAttachmentId))
  }
  lines.push(toCodexInputLineWithStaged(message, threadId, staged, model, contextMessages))
  return lines
}

function toCodexInputLineWithStaged(
  message: SlackbotV2ApiMessage,
  threadId: string,
  staged: Map<SlackbotV2ApiAttachment, string>,
  model?: string,
  contextMessages?: SlackbotV2ApiMessage[]
): string {
  return JSON.stringify({
    type: 'user',
    thread_key: threadId,
    trace_metadata: sessionMetadata(message, { action: 'execute' }),
    ...(model ? { model } : {}),
    message: {
      role: 'user',
      content: codexInputContent(message, staged, contextMessages)
    }
  })
}

function stagedAttachmentInputLines(
  attachment: SlackbotV2ApiAttachment,
  stagedAttachmentId: string
): string[] {
  const dataBase64 = attachment.dataBase64
  if (!dataBase64) return []
  const lines: string[] = []
  const chunkSize = STAGED_ATTACHMENT_CHUNK_CHARS - (STAGED_ATTACHMENT_CHUNK_CHARS % 4)
  for (let offset = 0, index = 0; offset < dataBase64.length; offset += chunkSize, index += 1) {
    const chunk = dataBase64.slice(offset, offset + chunkSize)
    lines.push(JSON.stringify({
      type: 'attachment.chunk',
      attachmentId: stagedAttachmentId,
      name: attachment.name,
      mimeType: attachment.mimeType,
      attachmentType: attachment.type,
      chunkIndex: index,
      final: offset + chunkSize >= dataBase64.length,
      dataBase64: chunk
    }))
  }
  return lines
}

function codexInputContent(
  message: SlackbotV2ApiMessage,
  staged: Map<SlackbotV2ApiAttachment, string> = new Map(),
  contextMessages?: SlackbotV2ApiMessage[]
): JsonValue[] {
  const content: JsonValue[] = []
  const threadContext = slackThreadContext(message, contextMessages)
  if (threadContext) {
    content.push({ type: 'text', text: threadContext })
  }
  if (message.text.trim()) {
    content.push({ type: 'text', text: message.text })
  }
  for (const attachment of message.attachments) {
    content.push(codexAttachmentInput(attachment, staged.get(attachment)))
  }
  return content.length > 0 ? content : [{ type: 'text', text: 'continue' }]
}

function slackThreadContext(
  currentMessage: SlackbotV2ApiMessage,
  contextMessages: SlackbotV2ApiMessage[] | undefined
): string | undefined {
  const priorMessages = (contextMessages ?? []).filter(message => message.id !== currentMessage.id)
  if (priorMessages.length === 0) return undefined

  const lines = [
    '# Slack Thread Context',
    '',
    'Earlier messages in this Slack thread, in chronological order:'
  ]
  for (const [index, message] of priorMessages.entries()) {
    const author = slackContextAuthor(message)
    const text = slackContextMessageText(message)
    lines.push('', `${index + 1}. ${author}:`, indentSlackContext(text || '[no text]'))
  }
  lines.push('', '# Current Request', '', 'The user message follows in the next content block.', '---')
  return lines.join('\n')
}

function slackContextAuthor(message: SlackbotV2ApiMessage): string {
  const displayName = message.author.fullName || message.author.userName || message.author.userId
  const userId = message.author.userId && message.author.userId !== displayName
    ? ` (${message.author.userId})`
    : ''
  const bot = message.author.isBot === true ? ' bot' : ''
  return `${displayName || 'unknown'}${userId}${bot}`
}

function slackContextMessageText(message: SlackbotV2ApiMessage): string {
  const fields = [message.text.trim()]
  for (const attachment of message.attachments) {
    fields.push(attachmentDescription(attachment))
  }
  return fields.filter(Boolean).join('\n')
}

function indentSlackContext(text: string): string {
  return text
    .split('\n')
    .map(line => `   ${line}`)
    .join('\n')
}

function codexAttachmentInput(
  attachment: SlackbotV2ApiAttachment,
  stagedAttachmentId?: string
): JsonValue {
  if (stagedAttachmentId) {
    return {
      type: 'attachment',
      attachment_type: attachment.type,
      stagedAttachmentId,
      name: attachment.name,
      mimeType: attachment.mimeType,
      size: attachment.size
    }
  }
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
  if (attachment.dataBase64) {
    return {
      type: 'attachment',
      attachment_type: attachment.type,
      dataBase64: attachment.dataBase64,
      mimeType: attachment.mimeType,
      name: attachment.name,
      size: attachment.size
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
    attachment.dataBase64Omitted ? `content=${attachment.dataBase64Omitted}` : undefined,
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
    if (event.event === 'session.execution_cancelled') {
      yield {
        data: { error: sessionErrorMessage(event, 'Execution cancelled') },
        event: event.event,
        eventId: event.id,
        eventKind: event.event
      } satisfies RustSessionStreamEvent
      return
    }
    if (event.event === 'session.execution_completed') {
      yield {
        data: sessionEventData(event),
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
    // Non-JSON stdout lines (e.g. sandbox bootstrap notices) are noise, not a
    // signal that the turn finished; treating them as terminal drops the answer.
    return false
  }
  if (!isJsonObject(payload)) return false

  return (
    payload.type === 'turn.completed' ||
    payload.type === 'turn.failed' ||
    payload.type === 'turn.done' ||
    payload.method === 'error' ||
    payload.method === 'turn/completed'
  )
}

function sessionEventData(event: ParsedSessionEvent): unknown {
  try {
    return JSON.parse(event.data)
  } catch {
    return event.data
  }
}

function sessionErrorMessage(event: ParsedSessionEvent, fallback?: string): string {
  let message = fallback ?? `${event.event ?? 'session error'}`
  try {
    const payload = JSON.parse(event.data)
    if (isJsonObject(payload)) {
      message = stringValue(payload.error) ?? stringValue(payload.message) ?? message
    }
  } catch {
    if (event.data.trim()) message = event.data.trim()
  }
  return message
}
