import type { AnyBlock } from '@slack/types'
import { describe, expect, it } from 'bun:test'
import { slackReplyLimits } from '../constants'
import {
  buildFinalFallbackText,
  estimatePayloadBytes,
  sanitizeFinalMessagePayload
} from './final-message'
import { planBlock } from './streaming'

describe('buildFinalFallbackText', () => {
  it('caps fallback near Slack guidance using displayed answer content', () => {
    const answer = 'x'.repeat(10_000)
    const text = buildFinalFallbackText({
      title: 'Centaur execution',
      answerMarkdown: answer,
      footer: 'Codex thread `T-1`'
    })
    expect(text.length).toBeLessThanOrEqual(slackReplyLimits.text.maxFallbackChars)
    expect(text.startsWith('Centaur execution')).toBe(true)
    expect(text.endsWith('…')).toBe(true)
    expect(text).not.toContain('x'.repeat(5000))
  })

  it('includes title and clipped answer without requiring a separate summary field', () => {
    const text = buildFinalFallbackText({
      title: 'Run',
      answerMarkdown: 'Done.',
      footer: undefined
    })
    expect(text).toBe('Run\nDone.')
  })
})

describe('sanitizeFinalMessagePayload', () => {
  it('keeps block count within the message limit', () => {
    const markdownBlocks = Array.from({ length: 60 }, (_, index) => ({
      type: 'markdown' as const,
      text: `section ${index}`
    }))
    const sanitized = sanitizeFinalMessagePayload(markdownBlocks)
    expect(sanitized.length).toBeLessThanOrEqual(slackReplyLimits.message.maxBlocks)
  })

  it('caps cumulative markdown characters across blocks', () => {
    const blocks = [
      { type: 'markdown' as const, text: 'a'.repeat(8_000) },
      { type: 'markdown' as const, text: 'b'.repeat(8_000) }
    ]
    const sanitized = sanitizeFinalMessagePayload(blocks)
    const total = sanitized
      .filter(block => block.type === 'markdown')
      .reduce((sum, block) => sum + (block as { text: string }).text.length, 0)
    expect(total).toBeLessThanOrEqual(slackReplyLimits.stream.markdownChunkChars)
  })

  it('shrinks oversized plan + markdown + footer compositions toward the byte budget', () => {
    const tasks = Array.from({ length: 12 }, (_, index) => ({
      id: `cmd-${index}`,
      title: `Run command ${index}`,
      status: 'complete' as const,
      details: {
        type: 'rich_text' as const,
        elements: [
          {
            type: 'rich_text_preformatted' as const,
            language: 'json',
            elements: [
              { type: 'text' as const, text: JSON.stringify({ index, blob: 'y'.repeat(400) }) }
            ]
          }
        ]
      },
      output: {
        type: 'rich_text' as const,
        elements: [
          {
            type: 'rich_text_preformatted' as const,
            language: 'json',
            elements: [
              { type: 'text' as const, text: JSON.stringify({ index, blob: 'z'.repeat(400) }) }
            ]
          }
        ]
      }
    }))
    const blocks = sanitizeFinalMessagePayload([
      planBlock('Centaur execution', tasks, 'plan-1') as AnyBlock,
      { type: 'markdown', text: 'Final answer ' + 'w'.repeat(8_000) },
      {
        type: 'context',
        elements: [{ type: 'mrkdwn', text: 'Codex thread `T-1`' }]
      }
    ])
    expect(estimatePayloadBytes(blocks)).toBeLessThanOrEqual(
      slackReplyLimits.mixedBodyAndPlan.maxPayloadBytes
    )
    expect(blocks.length).toBeLessThanOrEqual(slackReplyLimits.message.maxBlocks)
  })
})
