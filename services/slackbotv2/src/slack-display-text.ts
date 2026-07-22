export type SlackDisplayTextSource = 'text' | 'raw_blocks' | 'raw_attachments' | 'empty'

export type SlackDisplayText = {
  rawAttachmentCount: number
  rawBlockCount: number
  source: SlackDisplayTextSource
  text: string
}

export function slackRichTextMentionsUser(raw: unknown, userId: string | undefined): boolean {
  if (!userId) return false
  const mention = new RegExp(`<@${escapeRegExp(userId)}(?:\\|[^>]*)?>`, 'i')
  for (const record of slackMessageRecords(raw)) {
    for (const key of ['blocks', 'attachments']) {
      const value = record[key]
      if (Array.isArray(value) && richValueMentionsUser(value, userId, mention)) return true
    }
  }
  return false
}

const MAX_RAW_DISPLAY_TEXT_CHARS = 24_000

type UnknownRecord = Record<string, unknown>

export function renderSlackDisplayText(input: { raw: unknown; text: string }): SlackDisplayText {
  const records = slackMessageRecords(input.raw)
  const rawBlockCount = countArrayFields(records, 'blocks')
  const rawAttachmentCount = countArrayFields(records, 'attachments')

  if (input.text.trim()) {
    return {
      rawAttachmentCount,
      rawBlockCount,
      source: 'text',
      text: input.text
    }
  }

  const blockText = finalizeDisplayLines(collectRawBlockLines(records))
  if (blockText) {
    return {
      rawAttachmentCount,
      rawBlockCount,
      source: 'raw_blocks',
      text: blockText
    }
  }

  const attachmentText = finalizeDisplayLines(collectRawAttachmentLines(records))
  if (attachmentText) {
    return {
      rawAttachmentCount,
      rawBlockCount,
      source: 'raw_attachments',
      text: attachmentText
    }
  }

  return {
    rawAttachmentCount,
    rawBlockCount,
    source: 'empty',
    text: ''
  }
}

export function slackMessagePromptText(message: {
  displayText?: string
  displayTextSource?: SlackDisplayTextSource
  links?: PromptLink[]
  text: string
}): string {
  const source = message.displayTextSource
  const text =
    (source === 'raw_blocks' || source === 'raw_attachments') && message.displayText
      ? message.displayText
      : message.text
  const links = promptLinksText(message.links, text)
  return [text, links].filter(part => part.trim()).join('\n\n')
}

type PromptLink = {
  description?: string
  isSlackMessage?: boolean
  siteName?: string
  title?: string
  url: string
}

function promptLinksText(
  links: readonly PromptLink[] | undefined,
  existingText: string
): string {
  const normalized = normalizePromptLinks(links).filter(link => !existingText.includes(link.url))
  if (normalized.length === 0) return ''

  const hasSlackMessageLink = normalized.some(link => link.isSlackMessage)
  const lines = ['Links included in the Slack message:']
  if (hasSlackMessageLink) {
    lines.push(
      'If the request is context-dependent, inspect linked Slack message/thread links before responding.'
    )
  }
  for (const link of normalized) {
    lines.push(`- ${promptLinkLine(link)}`)
  }
  return lines.join('\n')
}

function normalizePromptLinks(
  links: readonly PromptLink[] | undefined
): PromptLink[] {
  const seen = new Set<string>()
  const normalized: PromptLink[] = []
  for (const link of links ?? []) {
    const url = link.url.trim()
    if (!url || seen.has(url)) continue
    seen.add(url)
    normalized.push({ ...link, url })
  }
  return normalized
}

function promptLinkLine(link: PromptLink): string {
  const fields = [
    link.isSlackMessage ? `Slack message/thread: ${link.url}` : link.url,
    link.title ? `Title: ${link.title}` : undefined,
    link.description ? `Description: ${link.description}` : undefined,
    link.siteName ? `Site: ${link.siteName}` : undefined
  ].filter(Boolean)
  return fields.join(' | ')
}

function slackMessageRecords(raw: unknown): UnknownRecord[] {
  const records: UnknownRecord[] = []
  const seen = new Set<UnknownRecord>()
  const add = (value: unknown): void => {
    if (!isRecord(value) || seen.has(value)) return
    records.push(value)
    seen.add(value)
  }

  add(raw)
  if (isRecord(raw)) {
    add(raw.event)
    add(raw.message)
    if (isRecord(raw.event)) add(raw.event.message)
  }
  return records
}

function countArrayFields(records: UnknownRecord[], key: string): number {
  let count = 0
  for (const record of records) {
    const value = record[key]
    if (Array.isArray(value)) count += value.length
  }
  return count
}

function collectRawBlockLines(records: UnknownRecord[]): string[] {
  const lines: string[] = []
  for (const record of records) {
    const blocks = record.blocks
    if (!Array.isArray(blocks)) continue
    for (const block of blocks) collectSlackBlockText(block, lines)
  }
  return lines
}

function collectRawAttachmentLines(records: UnknownRecord[]): string[] {
  const lines: string[] = []
  for (const record of records) {
    const attachments = record.attachments
    if (!Array.isArray(attachments)) continue
    for (const attachment of attachments) collectSlackAttachmentText(attachment, lines)
  }
  return lines
}

function richValueMentionsUser(value: unknown, userId: string, mention: RegExp): boolean {
  if (typeof value === 'string') return mention.test(value)
  if (Array.isArray(value)) {
    return value.some(item => richValueMentionsUser(item, userId, mention))
  }
  if (!isRecord(value)) return false
  if (value.type === 'user' && stringField(value.user_id).toUpperCase() === userId.toUpperCase()) {
    return true
  }
  return Object.values(value).some(item => richValueMentionsUser(item, userId, mention))
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function collectSlackAttachmentText(value: unknown, lines: string[]): void {
  if (!isRecord(value)) return
  collectStringFields(value, lines, ['fallback', 'pretext', 'title', 'text'])

  const fields = value.fields
  if (Array.isArray(fields)) {
    for (const field of fields) {
      if (!isRecord(field)) continue
      collectStringFields(field, lines, ['title', 'value'])
    }
  }

  const blocks = value.blocks
  if (Array.isArray(blocks)) {
    for (const block of blocks) collectSlackBlockText(block, lines)
  }
}

function collectSlackBlockText(value: unknown, lines: string[]): void {
  if (!isRecord(value)) return
  const type = stringField(value.type)

  if (type === 'section') {
    collectTextObject(value.text, lines)
    collectTextObjects(value.fields, lines)
    collectSlackElementText(value.accessory, lines)
    return
  }

  if (type === 'context') {
    collectSlackElementsText(value.elements, lines)
    return
  }

  if (type === 'header') {
    collectTextObject(value.text, lines)
    return
  }

  if (type === 'rich_text') {
    collectRichTextLines(value.elements, lines)
    return
  }

  if (type === 'image') {
    collectTextObject(value.title, lines)
    collectStringFields(value, lines, ['alt_text'])
    return
  }

  if (type === 'actions') {
    collectSlackElementsText(value.elements, lines)
    return
  }

  if (type === 'input') {
    collectTextObject(value.label, lines)
    collectTextObject(value.hint, lines)
    collectSlackElementText(value.element, lines)
    return
  }

  collectTextObject(value.text, lines)
  collectTextObjects(value.fields, lines)
  collectSlackElementsText(value.elements, lines)
}

function collectSlackElementsText(value: unknown, lines: string[]): void {
  if (!Array.isArray(value)) return
  for (const element of value) collectSlackElementText(element, lines)
}

function collectSlackElementText(value: unknown, lines: string[]): void {
  if (!isRecord(value)) return
  collectTextObject(value.text, lines)
  collectTextObject(value.placeholder, lines)
  collectTextObject(value.confirm, lines)
  collectStringFields(value, lines, ['alt_text'])
}

function collectTextObjects(value: unknown, lines: string[]): void {
  if (!Array.isArray(value)) return
  for (const item of value) collectTextObject(item, lines)
}

function collectTextObject(value: unknown, lines: string[]): void {
  if (typeof value === 'string') {
    lines.push(value)
    return
  }
  if (!isRecord(value)) return
  const text = value.text
  if (typeof text === 'string') lines.push(text)
}

function collectStringFields(
  record: UnknownRecord,
  lines: string[],
  fields: string[]
): void {
  for (const field of fields) {
    const value = record[field]
    if (typeof value === 'string') lines.push(value)
  }
}

function collectRichTextLines(value: unknown, lines: string[]): void {
  if (!Array.isArray(value)) return
  for (const element of value) {
    const text = richTextElementText(element)
    if (text) lines.push(text)
  }
}

function richTextElementText(value: unknown): string {
  if (!isRecord(value)) return ''
  const type = stringField(value.type)

  if (type === 'text') return stringField(value.text)
  if (type === 'link') {
    const text = stringField(value.text)
    const url = stringField(value.url)
    return text && url ? `${text} (${url})` : text || url
  }
  if (type === 'user') return slackRef('@', value.user_id)
  if (type === 'channel') return slackRef('#', value.channel_id)
  if (type === 'usergroup') return slackRef('@', value.usergroup_id)
  if (type === 'emoji') {
    const name = stringField(value.name)
    return name ? `:${name}:` : ''
  }

  const children = richTextChildren(value.elements)
  if (type === 'rich_text_section') return children.join('')
  if (type === 'rich_text_list') return children.map(child => `- ${child}`).join('\n')
  if (type === 'rich_text_quote') {
    return children
      .join('\n')
      .split('\n')
      .map(line => `> ${line}`)
      .join('\n')
  }
  if (type === 'rich_text_preformatted') return children.join('')
  return children.join('\n')
}

function richTextChildren(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  return value.map(richTextElementText).filter(Boolean)
}

function slackRef(prefix: string, value: unknown): string {
  const id = stringField(value)
  return id ? `${prefix}${id}` : ''
}

function finalizeDisplayLines(lines: string[]): string {
  const seen = new Set<string>()
  const normalized: string[] = []
  for (const line of lines) {
    for (const part of normalizeSlackFallbackText(line).split('\n')) {
      if (!part || seen.has(part)) continue
      seen.add(part)
      normalized.push(part)
    }
  }
  return truncateRawDisplayText(normalized.join('\n'))
}

function normalizeSlackFallbackText(input: string): string {
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
    .replace(/\r\n/g, '\n')
    .replace(/\r/g, '\n')
    .split('\n')
    .map(line => line.replace(/[ \t]+/g, ' ').trim())
    .filter(Boolean)
    .join('\n')
}

function truncateRawDisplayText(text: string): string {
  if (text.length <= MAX_RAW_DISPLAY_TEXT_CHARS) return text
  let omitted = text.length - MAX_RAW_DISPLAY_TEXT_CHARS
  while (true) {
    const suffix = `\n[truncated ${omitted} chars from Slack display text]`
    const keep = Math.max(0, MAX_RAW_DISPLAY_TEXT_CHARS - suffix.length)
    const actualOmitted = text.length - keep
    if (actualOmitted === omitted) return `${text.slice(0, keep).trimEnd()}${suffix}`
    omitted = actualOmitted
  }
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function isRecord(value: unknown): value is UnknownRecord {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}
