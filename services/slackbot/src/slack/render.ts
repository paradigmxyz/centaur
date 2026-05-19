import type { AnyBlock, AnyChunk, ContextBlock, MarkdownBlock, RichTextBlock } from '@slack/types'
import { slackReplyLimits } from '../constants'

const MAX_BLOCKS = slackReplyLimits.message.maxBlocks
const MAX_MARKDOWN_CHARS = slackReplyLimits.stream.markdownChunkChars
const MAX_FALLBACK_CHARS = slackReplyLimits.text.maxFallbackChars
const MAX_STREAM_CHUNK_CHARS = 4_000

export type StatusMetadata = {
  title?: string
  status?: string
  fields?: Record<string, string | number | boolean | null | undefined>
}

export function blockquoteMarkdown(text: string): string {
  return text
    .split('\n')
    .map(line => `> ${line}`)
    .join('\n')
}

/** Skip Thinking when Codex repeated the same prose in commentary and final_answer. */
export function shouldShowThinkingBlock(commentary: string, answer: string): boolean {
  const trimmedCommentary = commentary.trim()
  const trimmedAnswer = answer.trim()
  if (!trimmedCommentary) return false
  if (!trimmedAnswer) return true
  if (trimmedCommentary === trimmedAnswer) return false
  if (trimmedAnswer.includes(trimmedCommentary)) return false
  return true
}

export function thinkingContextBlock(
  commentary: string,
  opts: { heading?: boolean } = {}
): ContextBlock | null {
  const trimmed = commentary.trim()
  if (!trimmed) return null
  const maxChars = slackReplyLimits.message.thinkingContextChars
  const body =
    trimmed.length > maxChars ? `${trimmed.slice(0, maxChars - 13)}\n// truncated` : trimmed
  return {
    type: 'context',
    elements: [{ type: 'mrkdwn', text: opts.heading === false ? body : `*Thinking*\n${body}` }]
  }
}

export function renderMarkdownBlocks(markdown: string): MarkdownBlock[] {
  const normalized = markdown.trim() || ' '
  const blocks: MarkdownBlock[] = []
  let used = 0

  for (const chunk of splitText(normalized, MAX_MARKDOWN_CHARS)) {
    if (blocks.length >= MAX_BLOCKS) break
    const remaining = MAX_MARKDOWN_CHARS - used
    if (remaining <= 0) break
    const text = chunk.slice(0, remaining)
    used += text.length
    blocks.push({ type: 'markdown', text })
  }

  return blocks
}

export function renderStatusBlock(metadata: StatusMetadata): RichTextBlock | null {
  const elements: Array<{ type: 'text'; text: string; style?: { bold?: boolean } }> = []
  if (metadata.title) {
    elements.push({ type: 'text', text: metadata.title, style: { bold: true } })
  }
  if (metadata.status) {
    if (elements.length) elements.push({ type: 'text', text: '\n' })
    elements.push({ type: 'text', text: metadata.status })
  }
  for (const [key, value] of Object.entries(metadata.fields ?? {})) {
    if (value === undefined || value === null) continue
    if (elements.length) elements.push({ type: 'text', text: '\n' })
    elements.push({ type: 'text', text: `${key}: `, style: { bold: true } })
    elements.push({ type: 'text', text: String(value) })
  }
  if (!elements.length) return null

  return {
    type: 'rich_text',
    elements: [{ type: 'rich_text_section', elements }]
  }
}

export function enforceBlockLimits(blocks: AnyBlock[]): AnyBlock[] {
  return blocks.slice(0, MAX_BLOCKS)
}

export function fallbackText(input: {
  markdown?: string
  metadata?: StatusMetadata
  fallback?: string
}): string {
  const parts = [
    input.fallback,
    input.markdown,
    input.metadata?.title,
    input.metadata?.status,
    ...Object.entries(input.metadata?.fields ?? {}).map(([key, value]) =>
      value === undefined || value === null ? '' : `${key}: ${String(value)}`
    )
  ].filter(Boolean)

  const text = parts.join('\n').replace(/\s+/g, ' ').trim() || 'Centaur update'
  return text.length > MAX_FALLBACK_CHARS ? `${text.slice(0, MAX_FALLBACK_CHARS - 1)}…` : text
}

export function markdownToStreamChunks(markdown: string): AnyChunk[] {
  return splitText(markdown || ' ', MAX_STREAM_CHUNK_CHARS).map(text => ({
    type: 'markdown_text',
    text
  }))
}

function splitText(input: string, maxChars: number): string[] {
  const chunks: string[] = []
  let remaining = input
  while (remaining.length > maxChars) {
    const hard = remaining.slice(0, maxChars)
    const boundary = Math.max(
      hard.lastIndexOf('\n\n'),
      hard.lastIndexOf('\n'),
      hard.lastIndexOf(' ')
    )
    const take = boundary > maxChars * 0.5 ? boundary : maxChars
    chunks.push(remaining.slice(0, take))
    remaining = remaining.slice(take).trimStart()
  }
  if (remaining) chunks.push(remaining)
  return chunks
}
