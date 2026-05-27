import type { WebClient } from '@slack/web-api'
import { logWarn } from '../logging'
import type { NormalizedPart, NormalizedSlackEvent, SlackEnvelope, SlackMessageFile } from './types'

type SlackMessageEvent = {
  type?: string
  subtype?: string
  user?: string
  user_team?: string
  source_team?: string
  bot_id?: string
  app_id?: string
  bot_profile?: SlackBotProfile
  channel?: string
  channel_type?: string
  team?: string
  text?: string
  ts?: string
  thread_ts?: string
  event_ts?: string
  blocks?: unknown[]
  attachments?: SlackMessageAttachment[]
  files?: SlackMessageFile[]
}

type SlackThreadMessage = {
  type?: string
  subtype?: string
  user?: string
  bot_id?: string
  app_id?: string
  bot_profile?: SlackBotProfile
  text?: string
  ts?: string
  blocks?: unknown[]
  attachments?: SlackMessageAttachment[]
  files?: SlackMessageFile[]
}

type SlackBotProfile = {
  id?: string
  app_id?: string
  user_id?: string
  name?: string
  team_id?: string
}

type SlackMessageAttachment = {
  fallback?: string
  pretext?: string
  title?: string
  title_link?: string
  text?: string
  fields?: Array<{
    title?: string
    value?: string
  }>
  footer?: string
  blocks?: unknown[]
}

type SlackHistoryMessage = NonNullable<NormalizedSlackEvent['history_messages']>[number]

export async function normalizeSlackEnvelope(opts: {
  envelope: SlackEnvelope
  botUserId?: string
  botId?: string
  triggerBotAllowlist?: readonly string[]
  client: WebClient
}): Promise<NormalizedSlackEvent | null> {
  if (opts.envelope.type !== 'event_callback') return null
  const event = opts.envelope.event as SlackMessageEvent | undefined
  if (!event || !isMessageLikeEvent(event)) return null
  if (event.type === 'message' && event.subtype === 'file_share') return null
  if (event.subtype && event.subtype !== 'file_share' && event.subtype !== 'bot_message')
    return null
  if (!event.channel || !event.ts) return null
  if (isSelfBotMessage(event, opts)) return null
  if (isBotAuthoredMessage(event) && !isAllowedTriggerBotMessage(event, opts.triggerBotAllowlist))
    return null

  const actorId = slackActorId(event)
  if (!actorId) return null

  const teamId = opts.envelope.team_id ?? event.team
  if (!teamId) return null

  const threadTs = event.thread_ts ?? event.ts
  const textPart = slackMessageText(event, opts.botUserId)
  const parts: NormalizedPart[] = []
  if (textPart) parts.push({ type: 'text', text: textPart })

  for (const file of event.files ?? []) {
    const part = await fetchSlackFilePart(opts.client, file)
    if (part) parts.push(part)
  }
  const isMention =
    event.type === 'app_mention' ||
    Boolean(opts.botUserId && messageMentionsBot(event, opts.botUserId))
  const historyMessages = isMention
    ? await collectThreadHistorySafely({
        client: opts.client,
        channel: event.channel,
        threadTs,
        currentTs: event.ts,
        teamId,
        botUserId: opts.botUserId,
        botId: opts.botId
      })
    : []

  return {
    thread_key: `slack:${teamId}:${event.channel}:${threadTs}`,
    message_id: `slack:${teamId}:${event.channel}:${event.ts}`,
    team_id: teamId,
    recipient_team_id: recipientSlackTeamId(event) ?? teamId,
    user_id: actorId,
    channel_id: event.channel,
    thread_ts: threadTs,
    is_mention: isMention,
    parts,
    ...(historyMessages.length ? { history_messages: historyMessages } : {}),
    slack: {
      event_id: opts.envelope.event_id,
      event_ts: event.event_ts,
      message_ts: event.ts,
      enterprise_id: opts.envelope.enterprise_id,
      user_team: event.user_team,
      source_team: event.source_team,
      bot_id: event.bot_id,
      app_id: event.app_id ?? event.bot_profile?.app_id,
      bot_user_id: event.bot_profile?.user_id
    }
  }
}

function recipientSlackTeamId(event: SlackMessageEvent): string | undefined {
  for (const candidate of [event.user_team, event.source_team, event.team]) {
    if (typeof candidate === 'string' && candidate.trim()) return candidate.trim()
  }
  return undefined
}

function isMessageLikeEvent(event: SlackMessageEvent): boolean {
  return event.type === 'message' || event.type === 'app_mention'
}

async function collectThreadHistorySafely(opts: {
  client: WebClient
  channel: string
  threadTs: string
  currentTs: string
  teamId: string
  botUserId?: string
  botId?: string
}): Promise<SlackHistoryMessage[]> {
  try {
    return await collectThreadHistory(opts)
  } catch (error) {
    logWarn('slack_thread_history_collect_failed', {
      channel: opts.channel,
      thread_ts: opts.threadTs,
      error: error instanceof Error ? error.message : String(error)
    })
    return []
  }
}

async function collectThreadHistory(opts: {
  client: WebClient
  channel: string
  threadTs: string
  currentTs: string
  teamId: string
  botUserId?: string
  botId?: string
}): Promise<SlackHistoryMessage[]> {
  if (opts.currentTs === opts.threadTs) return []
  const history: SlackHistoryMessage[] = []
  let cursor: string | undefined

  do {
    const response = await opts.client.conversations.replies({
      channel: opts.channel,
      ts: opts.threadTs,
      limit: 200,
      cursor
    })
    const messages = Array.isArray(response.messages) ? response.messages : []
    for (const raw of messages) {
      const message = raw as SlackThreadMessage
      if (!message.ts || compareSlackTs(message.ts, opts.currentTs) >= 0) continue
      if (
        message.subtype &&
        message.subtype !== 'file_share' &&
        message.subtype !== 'bot_message'
      ) {
        continue
      }
      const role = isSelfBotMessage(message, opts) ? 'assistant' : 'user'
      const actorId = slackActorId(message)
      if (role === 'user' && !actorId) continue

      const parts = await partsFromSlackMessage(opts.client, message, opts.botUserId)
      if (!parts.length) continue
      history.push({
        message_id: `slack:${opts.teamId}:${opts.channel}:${message.ts}`,
        role,
        parts,
        user_id: actorId,
        metadata: { platform: 'slack', history_backfill: true }
      })
    }

    const nextCursor = response.response_metadata?.next_cursor
    cursor = typeof nextCursor === 'string' && nextCursor.trim() ? nextCursor : undefined
  } while (cursor)

  return history
}

async function partsFromSlackMessage(
  client: WebClient,
  message: SlackThreadMessage,
  botUserId?: string
): Promise<NormalizedPart[]> {
  const textPart = slackMessageText(message, botUserId)
  const parts: NormalizedPart[] = []
  if (textPart) parts.push({ type: 'text', text: textPart })

  for (const file of message.files ?? []) {
    const part = await fetchSlackFilePart(client, file)
    if (part) parts.push(part)
  }
  return parts
}

function slackMessageText(
  message: Pick<SlackMessageEvent, 'text' | 'blocks' | 'attachments'>,
  botUserId?: string
): string {
  return uniqueNonEmpty([
    preferRichText(message.text, message.blocks, botUserId),
    normalizeSlackAttachments(message.attachments, botUserId)
  ]).join('\n\n')
}

function messageMentionsBot(
  message: Pick<SlackMessageEvent, 'text'>,
  botUserId: string
): boolean {
  return typeof message.text === 'string' && message.text.includes(`<@${botUserId}>`)
}

function slackActorId(
  message: Pick<SlackMessageEvent, 'user' | 'bot_id' | 'bot_profile'>
): string | undefined {
  for (const candidate of [message.user, message.bot_profile?.user_id, message.bot_id]) {
    if (typeof candidate === 'string' && candidate.trim()) return candidate.trim()
  }
  return undefined
}

function isSelfBotMessage(
  message: Pick<SlackMessageEvent, 'user' | 'bot_id' | 'bot_profile'>,
  opts: { botUserId?: string; botId?: string }
): boolean {
  return Boolean(
    (opts.botUserId &&
      (message.user === opts.botUserId || message.bot_profile?.user_id === opts.botUserId)) ||
    (opts.botId && (message.bot_id === opts.botId || message.bot_profile?.id === opts.botId))
  )
}

function isBotAuthoredMessage(
  message: Pick<SlackMessageEvent, 'subtype' | 'bot_id' | 'bot_profile'>
): boolean {
  return Boolean(message.bot_id || message.bot_profile || message.subtype === 'bot_message')
}

function isAllowedTriggerBotMessage(
  message: Pick<SlackMessageEvent, 'user' | 'bot_id' | 'app_id' | 'bot_profile'>,
  allowlist: readonly string[] | undefined
): boolean {
  if (!allowlist?.length) return false
  const appIds = normalizedIdentifierSet(message.app_id, message.bot_profile?.app_id)
  const botIds = normalizedIdentifierSet(message.bot_id, message.bot_profile?.id)
  const botUserIds = normalizedIdentifierSet(message.user, message.bot_profile?.user_id)
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
  return new Set(
    values.map(value => value?.trim()).filter((value): value is string => Boolean(value))
  )
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

function uniqueNonEmpty(values: string[]): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const value of values) {
    const text = value.trim()
    if (!text || seen.has(text)) continue
    seen.add(text)
    out.push(text)
  }
  return out
}

function preferRichText(
  rawText: string | undefined,
  blocks: unknown[] | undefined,
  botUserId?: string
): string {
  const richText = normalizeRichTextBlocks(blocks)
  if (richText) return stripBotMention(richText, botUserId)
  return normalizeSlackText(rawText ?? '', botUserId)
}

function stripBotMention(text: string, botUserId?: string): string {
  if (!botUserId) return text.trim()
  return text.replaceAll(`@${botUserId}`, '').trim()
}

function normalizeSlackAttachments(
  attachments: SlackMessageAttachment[] | undefined,
  botUserId?: string
): string {
  if (!Array.isArray(attachments)) return ''
  const lines: string[] = []
  for (const attachment of attachments) {
    const blocksText = normalizeRichTextBlocks(attachment.blocks)
    if (blocksText) lines.push(stripBotMention(blocksText, botUserId))
    for (const value of [
      attachment.pretext,
      attachment.title,
      attachment.text,
      attachment.fallback,
      attachment.footer
    ]) {
      if (typeof value === 'string') lines.push(normalizeSlackText(value, botUserId))
    }
    for (const field of attachment.fields ?? []) {
      const title = typeof field.title === 'string' ? normalizeSlackText(field.title) : ''
      const value =
        typeof field.value === 'string' ? normalizeSlackText(field.value, botUserId) : ''
      if (title && value) lines.push(`${title}: ${value}`)
      else if (title || value) lines.push(title || value)
    }
  }
  return uniqueNonEmpty(lines).join('\n')
}

function compareSlackTs(a: string, b: string): number {
  const left = Number(a)
  const right = Number(b)
  if (Number.isFinite(left) && Number.isFinite(right)) return left - right
  return a.localeCompare(b)
}

export function normalizeSlackText(input: string, botUserId?: string): string {
  let text = input
  if (botUserId) text = text.replaceAll(`<@${botUserId}>`, '').trim()
  return text
    .replace(/<([a-z]+:\/\/[^>|]+)\|([^>]+)>/gi, '$2 ($1)')
    .replace(/<([a-z]+:\/\/[^>]+)>/gi, '$1')
    .replace(/<#([A-Z0-9]+)\|([^>]+)>/g, '#$2')
    .replace(/<#([A-Z0-9]+)>/g, '#$1')
    .replace(/<@([A-Z0-9]+)>/g, '@$1')
    .replace(/<!subteam\^([A-Z0-9]+)\|([^>]+)>/g, '@$2')
    .replace(/<!(channel|here|everyone)>/g, '@$1')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .trim()
}

function normalizeRichTextBlocks(blocks: unknown[] | undefined): string {
  if (!Array.isArray(blocks)) return ''
  return blocks.map(normalizeBlock).filter(Boolean).join('\n').trim()
}

function normalizeBlock(block: unknown): string {
  if (!isRecord(block)) return ''
  if (block.type === 'rich_text' && Array.isArray(block.elements)) {
    return block.elements.map(normalizeRichTextContainer).filter(Boolean).join('\n')
  }
  return ''
}

function normalizeRichTextContainer(container: unknown): string {
  if (!isRecord(container)) return ''
  if (container.type === 'rich_text_section' && Array.isArray(container.elements)) {
    return container.elements.map(normalizeRichTextElement).join('')
  }
  if (container.type === 'rich_text_list' && Array.isArray(container.elements)) {
    return container.elements.map(element => `- ${normalizeRichTextContainer(element)}`).join('\n')
  }
  if (container.type === 'rich_text_quote' && Array.isArray(container.elements)) {
    return container.elements.map(normalizeRichTextElement).join('')
  }
  if (container.type === 'rich_text_preformatted' && Array.isArray(container.elements)) {
    return container.elements.map(normalizeRichTextElement).join('')
  }
  return ''
}

function normalizeRichTextElement(element: unknown): string {
  if (!isRecord(element)) return ''
  switch (element.type) {
    case 'text':
      return typeof element.text === 'string' ? element.text : ''
    case 'link':
      return typeof element.text === 'string'
        ? `${element.text} (${stringField(element.url)})`
        : stringField(element.url)
    case 'user':
      return `@${stringField(element.user_id)}`
    case 'channel':
      return `#${stringField(element.channel_id)}`
    case 'emoji':
      return `:${stringField(element.name)}:`
    case 'broadcast':
      return `@${stringField(element.range)}`
    default:
      return ''
  }
}

async function fetchSlackFilePart(
  client: WebClient,
  file: SlackMessageFile
): Promise<NormalizedPart | null> {
  const url = file.url_private_download ?? file.url_private
  if (!url) return null
  const token = client.token
  if (!token) return null

  const response = await fetch(url, {
    headers: { Authorization: `Bearer ${token}` }
  })
  if (!response.ok) {
    throw new Error(
      `Slack file fetch failed for ${file.id ?? file.name ?? 'unknown'}: ${response.status}`
    )
  }

  const bytes = new Uint8Array(await response.arrayBuffer())
  const mimeType =
    file.mimetype ?? response.headers.get('content-type') ?? 'application/octet-stream'
  const type = mimeType.startsWith('image/')
    ? 'image'
    : isDocumentMime(mimeType)
      ? 'document'
      : 'file'
  return {
    type,
    name: file.name ?? file.title ?? file.id ?? 'slack-file',
    mime_type: mimeType,
    size: file.size ?? bytes.byteLength,
    slack_file_id: file.id,
    source: {
      type: 'base64',
      media_type: mimeType,
      data: Buffer.from(bytes).toString('base64')
    }
  }
}

function isDocumentMime(mimeType: string): boolean {
  return (
    mimeType.startsWith('text/') ||
    mimeType === 'application/pdf' ||
    mimeType.includes('document') ||
    mimeType.includes('spreadsheet') ||
    mimeType.includes('presentation') ||
    mimeType.includes('json')
  )
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : ''
}
