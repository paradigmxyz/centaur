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
  /**
   * Fires when session creation restarted the thread onto a new harness
   * (explicit --claude/--amp/--codex on a thread pinned to another harness).
   * Runs before append/execute, so the callback may set
   * `input.contextPreamble` to re-feed thread history to the fresh harness.
   */
  onSessionRestarted?(): Promise<void>
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

function rawSlackString(raw: unknown, key: string): string | undefined {
  if (!isJsonObject(raw)) return undefined
  return stringValue(raw[key])
}

export async function forwardToSessionApi(
  options: SlackbotV2Options,
  input: ForwardSessionInput,
  callbacks: ForwardSessionApiCallbacks = {}
): Promise<AsyncIterable<SlackbotV2RendererSource> | null> {
  const createStartedAtMs = nowMs()
  const created = await createSession(
    options,
    input.threadId,
    input.harnessType,
    sessionRequesterMessage(input)
  )
  traceLog(options, 'slackbotv2_session_create_complete', input.trace, {
    harness_switched: created.harnessSwitched,
    phase_ms: elapsedMs(createStartedAtMs)
  })
  if (created.harnessSwitched) {
    await callbacks.onSessionRestarted?.()
  }
  if (input.messages.length > 0) {
    const appendStartedAtMs = nowMs()
    await appendSessionMessages(options, input.threadId, input.messages, !input.executeMessage)
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
    input.executeContextMessages,
    input.contextPreamble
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

const RESTART_CONTEXT_MAX_CHARS = 24_000

/**
 * Transcript of the Slack thread, fed to a freshly restarted harness as a
 * context preamble (the old harness's conversation state dies with its
 * sandbox). The current message is excluded — it rides in the same input line
 * as the actual user turn.
 */
export function harnessRestartPreamble(
  history: SlackbotV2ApiMessage[],
  currentMessageId: string
): string | undefined {
  const lines: string[] = []
  for (const item of history) {
    if (item.id === currentMessageId) continue
    const text = item.text.trim()
    if (!text) continue
    const author = item.author.isMe
      ? 'assistant'
      : item.author.userName || item.author.fullName || 'user'
    lines.push(`[${author}]: ${text}`)
  }
  if (lines.length === 0) return undefined
  let transcript = lines.join('\n')
  if (transcript.length > RESTART_CONTEXT_MAX_CHARS) {
    transcript = `…(earlier messages truncated)\n${transcript.slice(-RESTART_CONTEXT_MAX_CHARS)}`
  }
  return (
    'This Slack thread was just restarted on a different agent harness, so the previous '
    + 'agent\'s working state is gone. Transcript of the thread so far, for context:\n'
    + transcript
  )
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

type RequesterIdentity = {
  githubHandle?: string
  githubHandleSource?: string
  githubUnavailableReason?: string
  slackDisplayName?: string
  slackMention?: string
  slackUserId?: string
  slackUserName?: string
}

type RequesterIdentityCacheEntry = {
  expiresAtMs: number
  identity: RequesterIdentity
}

const REQUESTER_IDENTITY_CACHE_SUCCESS_TTL_MS = 6 * 60 * 60 * 1000
const REQUESTER_IDENTITY_CACHE_MISS_TTL_MS = 10 * 60 * 1000
const requesterIdentityCache = new Map<string, RequesterIdentityCacheEntry>()

export function clearRequesterIdentityCacheForTests(): void {
  requesterIdentityCache.clear()
}

type ConversationNameCacheEntry = {
  expiresAtMs: number
  name: string | null
}

const CONVERSATION_NAME_CACHE_SUCCESS_TTL_MS = 6 * 60 * 60 * 1000
const CONVERSATION_NAME_CACHE_MISS_TTL_MS = 10 * 60 * 1000
const conversationNameCache = new Map<string, ConversationNameCacheEntry>()

export function clearConversationNameCacheForTests(): void {
  conversationNameCache.clear()
}

type CreateSessionOutcome = {
  /** The API restarted the thread onto the requested harness. */
  harnessSwitched: boolean
}

async function createSession(
  options: SlackbotV2Options,
  threadId: string,
  harnessType?: string,
  message?: SlackbotV2ApiMessage
): Promise<CreateSessionOutcome> {
  const requested = harnessType ?? options.defaultHarnessType ?? DEFAULT_HARNESS_TYPE
  // An explicit --claude/--amp/--codex restarts a thread pinned to another
  // harness; the implicit default never forces a switch.
  const response = await postCreateSession(
    options,
    threadId,
    requested,
    message,
    harnessType ? 'restart' : undefined
  )
  if (response.ok) {
    return { harnessSwitched: await harnessSwitchedFromResponse(response) }
  }

  let body = ''
  try {
    body = await response.text()
  } catch {
    body = ''
  }
  // A thread is pinned to the harness it was created with; the API rejects a
  // differing harness_type with 409. A plain message on a thread created with
  // a non-default harness lands here: keep the thread alive on its existing
  // harness instead of failing the message.
  const existing = response.status === 409 ? existingHarnessFromConflict(body) : undefined
  if (existing && existing !== requested) {
    const retry = await postCreateSession(options, threadId, existing, message)
    await ensureApiOk(retry, 'create session')
    return { harnessSwitched: false }
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
  harnessType: string,
  message?: SlackbotV2ApiMessage,
  onHarnessConflict?: 'reject' | 'restart'
): Promise<Response> {
  const fetchFn = options.fetch ?? fetch
  // The conversation name becomes the session principal's display name in
  // iron-control; resolve it here so it rides the create-session metadata that
  // api-rs reads when it registers the principal.
  const conversationName = message ? await resolveConversationName(options, message) : undefined
  const body: SlackbotV2CreateSessionRequest = {
    harness_type: harnessType,
    metadata: {
      source: 'slackbotv2',
      platform: 'slack',
      thread_id: threadId,
      ...sessionRequesterMetadata(message),
      ...(conversationName ? { slack_conversation_name: conversationName } : {})
    },
    ...(onHarnessConflict ? { on_harness_conflict: onHarnessConflict } : {})
  }
  return fetchFn(apiSessionUrl(options.apiUrl, threadId), {
    method: 'POST',
    headers: apiHeaders(options),
    body: JSON.stringify(body)
  })
}

async function harnessSwitchedFromResponse(response: Response): Promise<boolean> {
  try {
    const payload = await response.json()
    return isJsonObject(payload) && payload.harness_switched === true
  } catch {
    return false
  }
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

function sessionRequesterMessage(input: ForwardSessionInput): SlackbotV2ApiMessage | undefined {
  return input.executeMessage ?? input.messages.find(message => message.author.isMe !== true)
}

function sessionRequesterMetadata(
  message?: SlackbotV2ApiMessage,
  identity?: RequesterIdentity
): JsonObject {
  const slackUserId = identity?.slackUserId ?? messageRequesterUserId(message)
  const slackUserName = identity?.slackUserName ?? message?.author.userName
  const slackDisplayName = identity?.slackDisplayName ?? message?.author.fullName
  return {
    ...(slackUserId ? { slack_user_id: slackUserId } : {}),
    ...(slackUserName ? { slack_user_name: slackUserName } : {}),
    ...(slackDisplayName ? { slack_display_name: slackDisplayName } : {}),
    ...(identity?.githubHandle ? { github_handle: identity.githubHandle } : {})
  }
}

function messageRequesterUserId(message: SlackbotV2ApiMessage | undefined): string | undefined {
  if (!message) return undefined
  const rawUserId = rawSlackUserId(message.raw)
  const authorUserId = stringValue(message.author.userId)
  return authorUserId ?? rawUserId
}

function rawSlackUserId(raw: unknown): string | undefined {
  if (!isJsonObject(raw)) return undefined
  const directUser = stringValue(raw.user)
  if (directUser) return directUser
  const user = raw.user
  if (isJsonObject(user)) {
    return stringValue(user.id) ?? stringValue(user.user_id)
  }
  const botProfile = raw.bot_profile
  if (isJsonObject(botProfile)) return stringValue(botProfile.user_id)
  return undefined
}

async function resolveRequesterIdentity(
  options: SlackbotV2Options,
  message: SlackbotV2ApiMessage
): Promise<RequesterIdentity> {
  const slackUserId = messageRequesterUserId(message)
  const identity: RequesterIdentity = {
    slackDisplayName: stringValue(message.author.fullName),
    slackMention: slackUserId ? `<@${slackUserId}>` : undefined,
    slackUserId,
    slackUserName: stringValue(message.author.userName)
  }
  if (!identity.slackUserId) return identity

  const cacheKey = requesterIdentityCacheKey(message, identity.slackUserId)
  const cached = cacheKey ? cachedRequesterIdentity(cacheKey) : undefined
  if (cached) return mergeRequesterIdentity(identity, cached)

  const profile = await fetchSlackUserProfile(options, identity.slackUserId)
  if (!profile) {
    identity.githubUnavailableReason = 'Slack profile could not be fetched'
    cacheRequesterIdentity(cacheKey, identity)
    return identity
  }

  identity.slackDisplayName =
    stringValue(profile.display_name)
    ?? stringValue(profile.real_name)
    ?? stringValue(profile.name)
    ?? identity.slackDisplayName
  identity.slackUserName = stringValue(profile.name) ?? identity.slackUserName

  const github = extractGithubHandleFromSlackProfile(profile)
  if (github.handle) {
    identity.githubHandle = github.handle
    identity.githubHandleSource = github.source ?? 'Slack profile custom field'
  } else {
    identity.githubUnavailableReason = github.reason
  }
  cacheRequesterIdentity(cacheKey, identity)
  return identity
}

function requesterIdentityCacheKey(
  message: SlackbotV2ApiMessage,
  slackUserId: string
): string | undefined {
  const teamId = message.teamId || slackTeamId(message.raw) || rawSlackString(message.raw, 'team_id')
  return teamId ? `slack:${teamId}:${slackUserId}` : `slack:${slackUserId}`
}

function cachedRequesterIdentity(cacheKey: string): RequesterIdentity | undefined {
  const cached = requesterIdentityCache.get(cacheKey)
  if (!cached) return undefined
  if (cached.expiresAtMs <= Date.now()) {
    requesterIdentityCache.delete(cacheKey)
    return undefined
  }
  return cached.identity
}

function cacheRequesterIdentity(cacheKey: string | undefined, identity: RequesterIdentity): void {
  if (!cacheKey) return
  const ttlMs = identity.githubHandle
    ? REQUESTER_IDENTITY_CACHE_SUCCESS_TTL_MS
    : REQUESTER_IDENTITY_CACHE_MISS_TTL_MS
  requesterIdentityCache.set(cacheKey, {
    expiresAtMs: Date.now() + ttlMs,
    identity: { ...identity }
  })
}

function mergeRequesterIdentity(
  fallback: RequesterIdentity,
  cached: RequesterIdentity
): RequesterIdentity {
  return {
    ...fallback,
    ...cached,
    slackDisplayName: cached.slackDisplayName ?? fallback.slackDisplayName,
    slackMention: fallback.slackMention ?? cached.slackMention,
    slackUserId: fallback.slackUserId ?? cached.slackUserId,
    slackUserName: cached.slackUserName ?? fallback.slackUserName
  }
}

async function fetchSlackUserProfile(
  options: SlackbotV2Options,
  userId: string
): Promise<JsonObject | null> {
  const token = options.botToken
  if (!token) return null
  if (options.fetch && !options.slackApiUrl) return null
  try {
    const [userPayload, profilePayload] = await Promise.all([
      slackApiGet(options, 'users.info', { user: userId }),
      slackApiGet(options, 'users.profile.get', { include_labels: 'true', user: userId })
    ])
    const user = isJsonObject(userPayload?.user) ? userPayload.user : undefined
    const userProfile = isJsonObject(user?.profile) ? user.profile : undefined
    const profile = isJsonObject(profilePayload?.profile) ? profilePayload.profile : userProfile
    if (!user && !profile) return null
    return {
      ...(user ?? {}),
      ...(profile ?? {}),
      ...(profile?.fields ? { fields: profile.fields } : {}),
      ...(profile?.custom_fields ? { custom_fields: profile.custom_fields } : {})
    }
  } catch {
    return null
  }
}

// Resolve the human-readable name for the conversation a message belongs to,
// used as the session principal's display name. A 1:1 DM principal keys on the
// user, so its name is the DM partner's display name (already resolved for the
// requester); a channel/group principal keys on the channel, so its name is the
// channel name fetched from Slack. Returns undefined when no name can be
// resolved, so the principal falls back to its synthetic id-based name.
async function resolveConversationName(
  options: SlackbotV2Options,
  message: SlackbotV2ApiMessage
): Promise<string | undefined> {
  const conversationId = slackConversationId(message)
  const kind = slackConversationKind(conversationId)
  if (kind === 'dm') {
    const identity = await resolveRequesterIdentity(options, message)
    return identity.slackDisplayName ?? identity.slackUserName ?? undefined
  }
  if (kind === 'channel' && conversationId) {
    return (await fetchSlackChannelName(options, conversationId)) ?? undefined
  }
  return undefined
}

// The conversation (channel/DM) id, preferring the raw event's `channel` field
// and falling back to the conversation segment of the thread key
// (`<source>:[<team>:]<conversation>[:<ts>]`). Slack ids carry their type in the
// first letter: C/G are channels/groups, D is a direct message.
function slackConversationId(message: SlackbotV2ApiMessage): string | undefined {
  const fromRaw = rawSlackString(message.raw, 'channel')
  if (fromRaw) return fromRaw
  for (const segment of message.threadId.split(':').slice(1)) {
    const first = segment.charAt(0)
    if (first === 'C' || first === 'D' || first === 'G') return segment
  }
  return undefined
}

function slackConversationKind(
  conversationId: string | undefined
): 'channel' | 'dm' | undefined {
  const first = conversationId?.charAt(0)
  if (first === 'D') return 'dm'
  if (first === 'C' || first === 'G') return 'channel'
  return undefined
}

async function fetchSlackChannelName(
  options: SlackbotV2Options,
  channelId: string
): Promise<string | null> {
  const token = options.botToken
  if (!token) return null
  if (options.fetch && !options.slackApiUrl) return null

  const cached = conversationNameCache.get(channelId)
  if (cached && cached.expiresAtMs > Date.now()) return cached.name

  let name: string | null = null
  try {
    const payload = await slackApiGet(options, 'conversations.info', { channel: channelId })
    const channel = isJsonObject(payload?.channel) ? payload.channel : undefined
    name = stringValue(channel?.name_normalized) ?? stringValue(channel?.name) ?? null
  } catch {
    name = null
  }
  conversationNameCache.set(channelId, {
    expiresAtMs:
      Date.now() +
      (name ? CONVERSATION_NAME_CACHE_SUCCESS_TTL_MS : CONVERSATION_NAME_CACHE_MISS_TTL_MS),
    name
  })
  return name
}

async function slackApiGet(
  options: SlackbotV2Options,
  method: string,
  params: Record<string, string>
): Promise<JsonObject | null> {
  const url = slackApiMethodUrl(options.slackApiUrl, method)
  for (const [key, value] of Object.entries(params)) url.searchParams.set(key, value)
  const response = await fetch(url, {
    method: 'GET',
    headers: { authorization: `Bearer ${options.botToken}` }
  })
  const payload = await response.json()
  if (!response.ok || !isJsonObject(payload) || payload.ok === false) return null
  return payload
}

function slackApiMethodUrl(slackApiUrl: string | undefined, method: string): URL {
  return new URL(method, slackApiUrl ?? 'https://slack.com/api/')
}

const GITHUB_LABEL_RE = /\bgithub\b/i
const GITHUB_URL_RE = /github\.com\/([A-Za-z0-9-]{1,39})(?:[/?#]|$)/i
const GITHUB_PREFIX_RE = /\bgithub\s*[:=]\s*@?([A-Za-z0-9-]{1,39})\b/i
const GITHUB_HANDLE_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/

function extractGithubHandleFromSlackProfile(
  profile: JsonObject
): { handle?: string; source?: string; reason: string } {
  const fields = slackProfileCustomFields(profile)
  if (fields.length === 0) return { reason: 'no GitHub custom field found on Slack profile' }

  let sawGithubField = false
  for (const field of fields) {
    const labelMentionsGithub = GITHUB_LABEL_RE.test(field.label)
    const valueMentionsGithub = GITHUB_LABEL_RE.test(field.value)
    if (!labelMentionsGithub && !valueMentionsGithub) continue
    sawGithubField = true

    const source = field.label
      ? `Slack profile custom field "${field.label}"`
      : 'Slack profile custom field'
    const urlMatch = GITHUB_URL_RE.exec(field.value)
    const prefixedMatch = GITHUB_PREFIX_RE.exec(field.value)
    const handle =
      validGithubHandle(urlMatch?.[1] ?? '')
      ?? validGithubHandle(prefixedMatch?.[1] ?? '')
      ?? (labelMentionsGithub ? validGithubHandle(field.value) : undefined)
    if (handle) return { handle: `@${handle}`, source, reason: '' }
  }

  return {
    reason: sawGithubField
      ? 'GitHub profile field did not contain a valid GitHub handle'
      : 'no GitHub custom field found on Slack profile'
  }
}

function slackProfileCustomFields(profile: JsonObject): Array<{ label: string; value: string }> {
  const fields: Array<{ label: string; value: string }> = []
  collectSlackCustomFields(fields, profile.custom_fields)
  collectSlackCustomFields(fields, profile.fields)
  return fields
}

function collectSlackCustomFields(
  fields: Array<{ label: string; value: string }>,
  rawFields: unknown
): void {
  if (!isJsonObject(rawFields)) return
  for (const [key, rawValue] of Object.entries(rawFields)) {
    if (isJsonObject(rawValue)) {
      const value = stringValue(rawValue.value)
      if (value) {
        fields.push({
          label: stringValue(rawValue.label) ?? stringValue(rawValue.alt) ?? key,
          value
        })
      }
    } else {
      const value = stringValue(rawValue)
      if (value) fields.push({ label: key, value })
    }
  }
}

function validGithubHandle(value: string): string | undefined {
  const candidate = value.trim().replace(/^@/, '').replace(/\/+$/, '').split('/', 1)[0] ?? ''
  return GITHUB_HANDLE_RE.test(candidate) ? candidate : undefined
}

async function appendSessionMessages(
  options: SlackbotV2Options,
  threadId: string,
  messages: SlackbotV2ApiMessage[],
  includeRequesterContext = false
): Promise<void> {
  const fetchFn = options.fetch ?? fetch
  const body: SlackbotV2AppendMessagesRequest = {
    messages: await Promise.all(
      messages.map(message => toSessionMessage(options, message, includeRequesterContext))
    )
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
  contextMessages?: SlackbotV2ApiMessage[],
  contextPreamble?: string
): Promise<SlackbotV2ExecuteSessionResponse> {
  const fetchFn = options.fetch ?? fetch
  const requesterIdentity = await resolveRequesterIdentity(options, message)
  const body: SlackbotV2ExecuteSessionRequest = {
    idempotency_key: message.id,
    metadata: sessionMetadata(message, { action: 'execute' }, requesterIdentity),
    input_lines: toCodexInputLines(
      message,
      threadId,
      model,
      requesterIdentity,
      contextMessages,
      contextPreamble
    ),
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

async function toSessionMessage(
  options: SlackbotV2Options,
  message: SlackbotV2ApiMessage,
  includeRequesterContext: boolean
): Promise<SlackbotV2SessionMessage> {
  const requesterIdentity =
    includeRequesterContext && message.isMention && !message.author.isMe
      ? await resolveRequesterIdentity(options, message)
      : undefined
  return {
    client_message_id: message.id,
    role: message.author.isMe ? 'assistant' : 'user',
    parts: sessionMessageParts(message, requesterIdentity),
    metadata: sessionMetadata(message, {}, requesterIdentity)
  }
}

function sessionMessageParts(
  message: SlackbotV2ApiMessage,
  requesterIdentity?: RequesterIdentity
): JsonValue[] {
  const parts: JsonValue[] = []
  const requesterContext = requesterIdentityContext(requesterIdentity)
  if (requesterContext) {
    parts.push({ type: 'text', text: requesterContext })
  }
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
  extra: JsonObject = {},
  requesterIdentity?: RequesterIdentity
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
    ...sessionRequesterMetadata(message, requesterIdentity),
    ...extra
  }
}

function toCodexInputLines(
  message: SlackbotV2ApiMessage,
  threadId: string,
  model?: string,
  requesterIdentity?: RequesterIdentity,
  contextMessages?: SlackbotV2ApiMessage[],
  contextPreamble?: string
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
      requesterIdentity,
      contextMessages,
      contextPreamble
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
  lines.push(
    toCodexInputLineWithStaged(
      message,
      threadId,
      staged,
      model,
      requesterIdentity,
      contextMessages,
      contextPreamble
    )
  )
  return lines
}

function toCodexInputLineWithStaged(
  message: SlackbotV2ApiMessage,
  threadId: string,
  staged: Map<SlackbotV2ApiAttachment, string>,
  model?: string,
  requesterIdentity?: RequesterIdentity,
  contextMessages?: SlackbotV2ApiMessage[],
  contextPreamble?: string
): string {
  return JSON.stringify({
    type: 'user',
    thread_key: threadId,
    trace_metadata: sessionMetadata(message, { action: 'execute' }, requesterIdentity),
    ...(model ? { model } : {}),
    message: {
      role: 'user',
      content: codexInputContent(
        message,
        staged,
        requesterIdentity,
        contextMessages,
        contextPreamble
      )
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

function requesterIdentityContext(identity: RequesterIdentity | undefined): string | undefined {
  if (!identity?.slackUserId && !identity?.slackUserName && !identity?.githubHandle) return undefined

  const lines = [
    '# Requester Context',
    '',
    'The Slack user who prompted this turn is:',
    ...(identity.slackUserId ? [`- Slack user ID: ${identity.slackUserId}`] : []),
    ...(identity.slackMention ? [`- Slack mention: ${identity.slackMention}`] : []),
    ...(identity.slackUserName ? [`- Slack username: ${identity.slackUserName}`] : []),
    ...(identity.slackDisplayName ? [`- Slack display name: ${identity.slackDisplayName}`] : [])
  ]

  if (identity.githubHandle) {
    const githubLogin = identity.githubHandle.replace(/^@/, '')
    lines.push(
      `- GitHub handle from Slack profile: ${identity.githubHandle}`,
      `- GitHub handle source: ${identity.githubHandleSource ?? 'Slack profile custom field'}`,
      '- GitHub handle verified: yes',
      '',
      '## GitHub PR Attribution',
      '',
      '- If you create a GitHub PR for this Slack request, '
        + `the PR body MUST contain this standalone line: \`Prompted by: ${identity.githubHandle}\``,
      '- The credited prompter is the requester in this section, not the Slack thread OP/root author.',
      '- This is a GitHub PR body requirement, not a Slack response mention rule.',
      `- Assign the PR to the requester when possible: \`${githubLogin}\``
    )
  } else {
    lines.push(
      '- GitHub handle from Slack profile: unavailable',
      `- GitHub handle unavailable reason: ${identity.githubUnavailableReason ?? 'not resolved'}`,
      '- GitHub handle verified: no',
      '',
      '## GitHub PR Attribution',
      '',
      '- If you create a GitHub PR for this Slack request, do not infer a GitHub '
        + 'username from Slack display name, real name, or email.',
      '- Omit the `Prompted by` line unless a verified GitHub handle is present.'
    )
  }

  lines.push('', 'The user message follows in the next content block.', '---')
  return lines.join('\n')
}

function codexInputContent(
  message: SlackbotV2ApiMessage,
  staged: Map<SlackbotV2ApiAttachment, string> = new Map(),
  requesterIdentity?: RequesterIdentity,
  contextMessages?: SlackbotV2ApiMessage[],
  contextPreamble?: string
): JsonValue[] {
  const content: JsonValue[] = []
  const requesterContext = requesterIdentityContext(requesterIdentity)
  if (requesterContext) {
    content.push({ type: 'text', text: requesterContext })
  }
  if (contextPreamble?.trim()) {
    content.push({ type: 'text', text: contextPreamble })
  } else {
    const threadContext = slackThreadContext(message, contextMessages)
    if (threadContext) {
      content.push({ type: 'text', text: threadContext })
    }
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
