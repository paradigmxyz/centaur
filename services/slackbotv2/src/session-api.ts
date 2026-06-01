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
  SlackbotV2Options,
  SlackbotV2RendererSource,
  SlackbotV2SessionMessage
} from './types'
import { elapsedMs, isJsonObject, nowMs, stringValue, toAsyncIterable, traceLog } from './utils'

export async function collectInitialContext(
  thread: { allMessages: AsyncIterable<Message> },
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
    text: message.text,
    threadId: message.threadId,
    timestamp: message.metadata.dateSent.toISOString()
  }
}

export async function forwardToSessionApi(
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

export function startingStreamNotification(threadId: string): JsonObject {
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

export function sessionStreamError(error: unknown): RustSessionStreamEvent {
  return {
    data: { error: error instanceof Error ? error.message : String(error) },
    event: 'session.stream_error',
    eventKind: 'session.stream_error'
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

function sessionMessageParts(message: SlackbotV2ApiMessage): JsonValue[] {
  const parts: JsonValue[] = []
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

function codexInputContent(message: SlackbotV2ApiMessage): JsonValue[] {
  const content: JsonValue[] = []
  if (message.text.trim()) {
    content.push({ type: 'text', text: message.text })
  }
  for (const attachment of message.attachments) {
    content.push(codexAttachmentInput(attachment))
  }
  return content.length > 0 ? content : [{ type: 'text', text: 'continue' }]
}

function codexAttachmentInput(attachment: SlackbotV2ApiAttachment): JsonValue {
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
    // TODO: Upload files through POST /session/{thread_key}/attachments and pass refs here.
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
  if (!isJsonObject(payload)) return false

  return (
    payload.type === 'turn.completed' ||
    payload.type === 'turn.failed' ||
    payload.type === 'turn.done' ||
    payload.method === 'error' ||
    payload.method === 'turn/completed'
  )
}

function sessionErrorMessage(event: ParsedSessionEvent): string {
  let message = `${event.event ?? 'session error'}`
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
