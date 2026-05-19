import type { AnyBlock, MarkdownBlock } from '@slack/types'
import { slackReplyLimits } from '../constants'
import { enforceBlockLimits } from './render'

const MARKDOWN_CUMULATIVE_CHARS = slackReplyLimits.stream.markdownChunkChars
const PAYLOAD_BYTE_BUDGET = slackReplyLimits.mixedBodyAndPlan.maxPayloadBytes
const MAX_FALLBACK_CHARS = slackReplyLimits.text.maxFallbackChars

export function buildFinalFallbackText(opts: {
  title: string
  answerMarkdown: string
  commentaryMarkdown?: string
}): string {
  const parts = [
    opts.title.trim(),
    opts.commentaryMarkdown?.trim(),
    opts.answerMarkdown.trim()
  ].filter(Boolean)
  const text = parts.join('\n')
  if (!text) return opts.title.trim() || 'Centaur update'
  if (text.length <= MAX_FALLBACK_CHARS) return text
  return `${text.slice(0, MAX_FALLBACK_CHARS - 1)}…`
}

/** Enforce Slack limits on the composed chat.update payload (plan + markdown + footer). */
export function sanitizeFinalMessagePayload(blocks: AnyBlock[]): AnyBlock[] {
  let sanitized = enforceBlockLimits(blocks)
  sanitized = capMarkdownBlocksCumulative(sanitized, MARKDOWN_CUMULATIVE_CHARS)
  sanitized = shrinkToPayloadByteBudget(sanitized, PAYLOAD_BYTE_BUDGET)
  return sanitized
}

function capMarkdownBlocksCumulative(blocks: AnyBlock[], maxChars: number): AnyBlock[] {
  let total = markdownCharsUsed(blocks)
  if (total <= maxChars) return blocks

  const out = blocks.map(block => ({ ...block })) as AnyBlock[]
  for (let index = out.length - 1; index >= 0 && total > maxChars; index -= 1) {
    const block = out[index]
    if (block?.type !== 'markdown') continue
    const markdown = block as MarkdownBlock
    const overflow = total - maxChars
    const nextLength = Math.max(0, markdown.text.length - overflow)
    const trimmed = markdown.text.slice(0, nextLength).trimEnd()
    markdown.text = trimmed || ' '
    total = markdownCharsUsed(out)
  }
  return out
}

function shrinkToPayloadByteBudget(blocks: AnyBlock[], maxBytes: number): AnyBlock[] {
  let sanitized = blocks
  let bytes = estimatePayloadBytes(sanitized)
  if (bytes <= maxBytes) return sanitized

  sanitized = trimMarkdownFromEnd(sanitized, 0.25)
  bytes = estimatePayloadBytes(sanitized)
  if (bytes <= maxBytes) return sanitized

  sanitized = dropTrailingMarkdownBlocks(sanitized)
  bytes = estimatePayloadBytes(sanitized)
  if (bytes <= maxBytes) return sanitized

  sanitized = stripPlanTaskBodies(sanitized)
  bytes = estimatePayloadBytes(sanitized)
  if (bytes <= maxBytes) return sanitized

  sanitized = trimPlanTasks(sanitized, slackReplyLimits.finalPlan.maxTasks)
  return sanitized
}

function trimMarkdownFromEnd(blocks: AnyBlock[], fraction: number): AnyBlock[] {
  const out = blocks.map(block => ({ ...block })) as AnyBlock[]
  for (let index = out.length - 1; index >= 0; index -= 1) {
    const block = out[index]
    if (block?.type !== 'markdown') continue
    const markdown = block as MarkdownBlock
    const keep = Math.max(1, Math.floor(markdown.text.length * (1 - fraction)))
    markdown.text = markdown.text.slice(0, keep).trimEnd() || ' '
    if (estimatePayloadBytes(out) <= PAYLOAD_BYTE_BUDGET) break
  }
  return out
}

function dropTrailingMarkdownBlocks(blocks: AnyBlock[]): AnyBlock[] {
  const out = [...blocks]
  while (out.length > 0 && estimatePayloadBytes(out) > PAYLOAD_BYTE_BUDGET) {
    const lastIndex = out.length - 1
    if (out[lastIndex]?.type === 'markdown') {
      out.pop()
      continue
    }
    break
  }
  return out
}

function stripPlanTaskBodies(blocks: AnyBlock[]): AnyBlock[] {
  return blocks.map(block => {
    if (block.type !== 'plan' || !('tasks' in block) || !Array.isArray(block.tasks)) {
      return block
    }
    return {
      ...block,
      tasks: block.tasks.map(task => ({
        ...task,
        details: undefined,
        output: undefined
      }))
    }
  })
}

function trimPlanTasks(blocks: AnyBlock[], maxTasks: number): AnyBlock[] {
  return blocks.map(block => {
    if (block.type !== 'plan' || !('tasks' in block) || !Array.isArray(block.tasks)) {
      return block
    }
    return {
      ...block,
      tasks: block.tasks.slice(0, maxTasks)
    }
  }) as AnyBlock[]
}

function markdownCharsUsed(blocks: AnyBlock[]): number {
  return blocks.reduce((total, block) => {
    if (block.type !== 'markdown') return total
    return total + (block as MarkdownBlock).text.length
  }, 0)
}

export function estimatePayloadBytes(blocks: AnyBlock[]): number {
  return Buffer.byteLength(JSON.stringify(blocks), 'utf8')
}
