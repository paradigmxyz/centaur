import * as Lark from '@larksuiteoapi/node-sdk'

const MAX_TEXT_CHARS = 40_000

export type FeishuClientConfig = {
  appId: string
  appSecret: string
  appType: Lark.AppType.SelfBuild
  domain: Lark.Domain.Feishu
}

export function buildFeishuClientConfig(
  appId: string,
  appSecret: string
): FeishuClientConfig {
  if (!appId.trim() || !appSecret.trim()) throw new Error('Feishu app credentials are required')
  return {
    appId,
    appSecret,
    appType: Lark.AppType.SelfBuild,
    domain: Lark.Domain.Feishu
  }
}

export type FeishuCommand = 'new' | 'projects'

export type NormalizedFeishuMessage = {
  eventId: string
  messageId: string
  tenantKey: string
  chatId: string
  conversationKey: string
  rootMessageId: string
  senderOpenId: string
  senderUnionId?: string
  principalId: string
  text: string
  command?: FeishuCommand
  isDirect: boolean
}

export type NormalizedFeishuCardAction = {
  eventId: string
  messageId: string
  tenantKey: string
  operatorOpenId: string
  operatorUnionId?: string
  action: string
  value: UnknownRecord
  formValue: UnknownRecord
}

type UnknownRecord = Record<string, unknown>

export function normalizeFeishuMessage(
  input: unknown,
  options: { botOpenId: string }
): NormalizedFeishuMessage | null {
  const envelope = record(input)
  if (!envelope) return null
  const header = record(envelope.header)
  const event = record(envelope.event) ?? envelope
  if (!event) return null
  const sender = record(event.sender)
  const senderId = record(sender?.sender_id)
  const message = record(event.message)
  if (!sender || !senderId || !message) return null

  const senderType = string(sender.sender_type)
  const senderOpenId = string(senderId.open_id)
  const botOpenId = options.botOpenId.trim()
  if (!senderOpenId || senderType !== 'user' || senderOpenId === botOpenId) return null

  const eventId = requiredString(header?.event_id ?? event.event_id, 'event_id')
  const tenantKey = requiredString(header?.tenant_key ?? event.tenant_key, 'tenant_key')
  const messageId = requiredString(message.message_id, 'message_id')
  const chatId = requiredString(message.chat_id, 'chat_id')
  const chatType = requiredString(message.chat_type, 'chat_type')
  if (message.message_type !== 'text') return null
  const text = parseTextContent(message.content)
  const mentions = array(message.mentions).flatMap(item => {
    const mention = record(item)
    const id = record(mention?.id)
    const key = string(mention?.key)
    const openId = string(id?.open_id)
    return key && openId ? [{ key, openId }] : []
  })
  const isDirect = chatType === 'p2p'
  if (!isDirect && !mentions.some(mention => mention.openId === botOpenId)) return null
  const normalizedText = normalizeText(
    text,
    isDirect ? [] : mentions.filter(mention => mention.openId === botOpenId).map(item => item.key)
  )
  if (!normalizedText) return null

  const senderUnionId = string(senderId.union_id)
  return {
    eventId,
    messageId,
    tenantKey,
    chatId,
    conversationKey: isDirect ? senderOpenId : chatId,
    rootMessageId: isDirect ? 'direct' : string(message.root_id) ?? messageId,
    senderOpenId,
    ...(senderUnionId ? { senderUnionId } : {}),
    principalId: `feishu:${tenantKey}:${senderOpenId}`,
    text: normalizedText,
    ...(command(normalizedText) ? { command: command(normalizedText) } : {}),
    isDirect
  }
}

export function normalizeFeishuCardAction(input: unknown): NormalizedFeishuCardAction | undefined {
  const raw = record(input)
  const context = record(raw?.context)
  const operator = record(raw?.operator)
  const operatorId = record(operator?.operator_id)
  const action = record(raw?.action)
  const value = record(action?.value)
  const eventId = string(raw?.event_id) ?? string(raw?.token)
  const messageId = string(context?.open_message_id) ?? string(raw?.open_message_id)
  const tenantKey = string(operator?.tenant_key)
    ?? string(raw?.operator_tenant_key)
    ?? string(raw?.tenant_key)
  const operatorOpenId = string(operatorId?.open_id)
    ?? string(operator?.open_id)
    ?? string(raw?.open_id)
  const operatorUnionId = string(operatorId?.union_id) ?? string(operator?.union_id)
  const actionName = string(value?.action)
  if (!eventId || !messageId || !tenantKey || !operatorOpenId || !actionName || !value) {
    return undefined
  }
  return {
    eventId,
    messageId,
    tenantKey,
    operatorOpenId,
    ...(operatorUnionId ? { operatorUnionId } : {}),
    action: actionName,
    value,
    formValue: record(action?.form_value) ?? record(raw?.form_value) ?? {}
  }
}

function parseTextContent(input: unknown): string {
  const raw = requiredString(input, 'message content')
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    throw new Error('Feishu text content is invalid')
  }
  const text = requiredString(record(parsed)?.text, 'message text')
  if ([...text].length > MAX_TEXT_CHARS) throw new Error('Feishu message text is too long')
  return text
}

function normalizeText(text: string, botMentionKeys: string[]): string {
  let normalized = text
  for (const key of botMentionKeys) normalized = normalized.replaceAll(key, ' ')
  return normalized.replace(/\s+/gu, ' ').trim()
}

function command(text: string): FeishuCommand | undefined {
  const normalized = text.trim().toLowerCase()
  if (normalized === '/new') return 'new'
  if (normalized === '/projects') return 'projects'
  return undefined
}

function record(input: unknown): UnknownRecord | undefined {
  return input !== null && typeof input === 'object' && !Array.isArray(input)
    ? input as UnknownRecord
    : undefined
}

function array(input: unknown): unknown[] {
  return Array.isArray(input) ? input : []
}

function string(input: unknown): string | undefined {
  return typeof input === 'string' && input.trim() ? input : undefined
}

function requiredString(input: unknown, name: string): string {
  const value = string(input)
  if (!value) throw new Error(`Feishu ${name} is required`)
  return value
}
