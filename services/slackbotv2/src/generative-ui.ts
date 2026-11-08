/**
 * Generative UI cards for Slack final answers, built with json-render on top
 * of the Chat SDK card model.
 *
 * Contract with the agent (only when `SLACKBOTV2_GENERATIVE_UI_ENABLED` is
 * on): the final answer may carry one fenced block tagged `json-render` whose
 * body is either a complete flat json-render spec (`{ root, elements }`) or
 * the JSONL patch stream `catalog.prompt()` teaches. slackbotv2 removes the
 * block from the markdown it streams to Slack and posts the card as a second
 * message in the thread. Anything else in the answer renders as today.
 *
 * Buttons that trigger actions are excluded from the catalog: slackbotv2 only
 * accepts `block_actions` for workflow buttons, so a card button would be a
 * dead control. `LinkButton` stays.
 */

import { compileSpecStream, defineCatalog, type Spec } from '@json-render/core'
import {
  CardSpecError,
  schema,
  standardComponentDefinitions,
  toCard,
  type CardLimits
} from '@json-render/chat'
import type { CardElement } from 'chat'
import type { ChatSDKStreamChunk } from '@centaur/rendering'

export const GENERATIVE_UI_FENCE_LANGUAGE = 'json-render'

export type GenerativeUiLimits = CardLimits

export type GenerativeUiOptions = {
  enabled: boolean
  /** Per-message limits `toCard` enforces instead of letting Slack degrade content. */
  limits?: GenerativeUiLimits
}

/** Slack degrades a second table into a code block; reject the spec instead. */
export const DEFAULT_GENERATIVE_UI_LIMITS: GenerativeUiLimits = { tables: 1, charts: 2 }

const LIMIT_KEYS: ReadonlySet<keyof GenerativeUiLimits> = new Set([
  'tables',
  'charts',
  'fields',
  'tableRows',
  'tableColumns',
  'chartLabel',
  'chartTitle',
  'chartSeries',
  'chartPoints'
])

const { Button: _interactiveButton, ...generativeUiComponents } = standardComponentDefinitions
void _interactiveButton

/** The catalog the agent is told about and the one `toCard` validates against. */
export const generativeUiCatalog = defineCatalog(schema, { components: generativeUiComponents })

const FENCE_RULES = [
  'Centaur delivers your answer as Slack markdown. To add a card, put the JSONL patches above (or one complete flat spec object with "root" and "elements") inside a single fenced code block tagged json-render anywhere in your final answer. This fence is the one exception to the no-code-fences rule; prose outside the fence stays markdown.',
  'Use at most one json-render block per answer. Only use a card when the data has a shape a component serves better than prose: row data, a trend, or a few key figures.',
  'Do not emit a card when the user asked for plain text only, no interactive blocks or no dashboards.',
  'Buttons that trigger actions are not available. For next steps, use LinkButton with a URL.'
]

/**
 * The system-prompt text describing the catalog, generated from the catalog
 * itself so the component list never drifts from what `toCard` accepts.
 */
export function generativeUiCatalogPrompt(limits: GenerativeUiLimits = DEFAULT_GENERATIVE_UI_LIMITS): string {
  return generativeUiCatalog.prompt({ customRules: [...FENCE_RULES, limitsRule(limits)] })
}

function limitsRule(limits: GenerativeUiLimits): string {
  const entries = Object.entries(limits).filter(([, value]) => typeof value === 'number')
  if (entries.length === 0) return 'Keep every component within the Slack limits for one message.'
  const described = entries.map(([key, value]) => `${key}: ${value}`).join(', ')
  return `Per-message limits (a spec beyond them is rejected and shown as code): ${described}.`
}

const COMPONENTS_HEADING = /^AVAILABLE COMPONENTS \(\d+\):$/m

/**
 * The component list of the generated catalog prompt: from its
 * "AVAILABLE COMPONENTS" heading to the next blank line.
 */
export function generativeUiComponentList(): string {
  const prompt = generativeUiCatalog.prompt()
  const heading = COMPONENTS_HEADING.exec(prompt)
  if (!heading) throw new Error('catalog prompt has no AVAILABLE COMPONENTS section')
  // The heading is followed by one blank line, then one line per component.
  const start = heading.index
  const listStart = start + heading[0].length + 2
  const end = prompt.indexOf('\n\n', listStart)
  return prompt.slice(start, end === -1 ? undefined : end).trimEnd()
}

/**
 * The note slackbotv2 prepends to a Slack user turn when the flag is on, so
 * the agent learns that a card is available without any persona or system
 * prompt change. Other ingresses never send it, so no other surface sees
 * the instruction. Kept short because it rides on every execute turn: the
 * component list and structural rules come from the catalog, the rest is the
 * fenced-block contract.
 */
export function generativeUiTurnNote(limits: GenerativeUiLimits = DEFAULT_GENERATIVE_UI_LIMITS): string {
  const rules = (generativeUiCatalog.schema.defaultRules ?? []).filter(
    rule => !/\$state|\$bindState/.test(rule)
  )
  return [
    '# Slack card (generative UI)',
    '',
    'This Slack surface can render one native card next to your markdown answer.',
    'Use it only when the data has a shape a component serves better than prose: row data (Table), a trend or a share of a whole (Chart), or a few key figures (Fields). Plain answers, questions and code need no card.',
    'Do not use a card when the user asked for plain text only, no interactive blocks or no dashboards.',
    'To add a card, put ONE fenced code block tagged json-render anywhere in your final answer. Its body is a single JSON object: {"root": "<key>", "elements": {"<key>": {"type": "<Component>", "props": {...}, "children": ["<key>", ...]}, ...}}. The block is removed from the message and the card is posted right after your prose. Everything outside the fence stays markdown.',
    `An invalid spec is shown as code instead of a card. ${limitsRule(limits)} Buttons that trigger actions are not available; use LinkButton with a URL.`,
    '',
    generativeUiComponentList(),
    '',
    'Rules:',
    ...rules.map(rule => `- ${rule}`),
    '---'
  ].join('\n')
}

/**
 * Parses `SLACKBOTV2_GENERATIVE_UI_LIMITS`: a JSON object of positive integer
 * limits keyed by the `CardLimits` fields. Unset keeps the defaults; anything
 * else is a startup error, like the other boolean/number env parsers.
 */
export function parseGenerativeUiLimits(raw: string | undefined): GenerativeUiLimits {
  const trimmed = raw?.trim()
  if (!trimmed) return { ...DEFAULT_GENERATIVE_UI_LIMITS }
  let parsed: unknown
  try {
    parsed = JSON.parse(trimmed)
  } catch {
    throw new Error('SLACKBOTV2_GENERATIVE_UI_LIMITS must be a JSON object')
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('SLACKBOTV2_GENERATIVE_UI_LIMITS must be a JSON object')
  }
  const limits: GenerativeUiLimits = {}
  for (const [key, value] of Object.entries(parsed)) {
    if (!LIMIT_KEYS.has(key as keyof GenerativeUiLimits)) {
      throw new Error(`SLACKBOTV2_GENERATIVE_UI_LIMITS: unknown limit "${key}"`)
    }
    if (typeof value !== 'number' || !Number.isInteger(value) || value <= 0) {
      throw new Error(`SLACKBOTV2_GENERATIVE_UI_LIMITS: "${key}" must be a positive integer`)
    }
    limits[key as keyof GenerativeUiLimits] = value
  }
  return limits
}

export type GenerativeUiExtraction = {
  /** The answer with the block removed when exactly one block was found; otherwise unchanged. */
  markdown: string
  /** Body of the first json-render block, when exactly one was found. */
  block?: string
  blockCount: number
}

// A fence line is three backticks followed by the language tag on its own line.
const OPEN_FENCE = /^```[ \t]*json-render[ \t]*$/m
const CLOSE_FENCE = /^```[ \t]*$/m

/**
 * Finds fenced `json-render` blocks in a complete markdown answer. Only an
 * answer with exactly one block has it removed from the markdown; with more
 * than one, every block stays in place so Slack renders them as code.
 */
export function extractGenerativeUiBlocks(markdown: string): GenerativeUiExtraction {
  const blocks: Array<{ body: string; end: number; start: number }> = []
  let cursor = 0
  while (cursor <= markdown.length) {
    const rest = markdown.slice(cursor)
    const open = OPEN_FENCE.exec(rest)
    if (!open) break
    const bodyStart = cursor + open.index + open[0].length + 1
    const close = CLOSE_FENCE.exec(markdown.slice(bodyStart))
    if (!close) break
    const bodyEnd = bodyStart + close.index
    const fenceEnd = bodyEnd + close[0].length
    blocks.push({
      body: markdown.slice(bodyStart, bodyEnd).replace(/\n$/, ''),
      end: markdown[fenceEnd] === '\n' ? fenceEnd + 1 : fenceEnd,
      start: cursor + open.index
    })
    cursor = fenceEnd
  }
  if (blocks.length !== 1) return { markdown, blockCount: blocks.length }
  const [block] = blocks
  const before = markdown.slice(0, block!.start)
  const after = markdown.slice(block!.end)
  return {
    block: block!.body,
    blockCount: 1,
    markdown: joinAroundBlock(before, after)
  }
}

function joinAroundBlock(before: string, after: string): string {
  if (!before.trim()) return after.replace(/^\n+/, '')
  if (!after.trim()) return before.replace(/\n+$/, '')
  return `${before.replace(/\n+$/, '')}\n\n${after.replace(/^\n+/, '')}`
}

export type GenerativeUiFailureReason = 'invalid_spec' | 'malformed_json' | 'multiple_blocks' | 'post_failed'

export class GenerativeUiSpecError extends Error {
  constructor(
    readonly reason: Exclude<GenerativeUiFailureReason, 'post_failed'>,
    readonly issues: string[]
  ) {
    super(`generative UI spec rejected (${reason}): ${issues.join('; ')}`)
    this.name = 'GenerativeUiSpecError'
  }
}

/**
 * Turns a block body into a json-render spec. A JSON object is taken as a
 * flat spec; otherwise every non-empty line must be a JSON Patch operation
 * (the JSONL format `catalog.prompt()` describes) and the lines are compiled.
 */
export function parseGenerativeUiSpec(block: string): Spec {
  const trimmed = block.trim()
  if (!trimmed) throw new GenerativeUiSpecError('malformed_json', ['empty json-render block'])
  const lines = trimmed.split('\n').map(line => line.trim()).filter(Boolean)
  if (lines.length === 1 || trimmed.startsWith('{') && !looksLikePatch(lines[0]!)) {
    let parsed: unknown
    try {
      parsed = JSON.parse(trimmed)
    } catch (error) {
      throw new GenerativeUiSpecError('malformed_json', [errorText(error)])
    }
    if (looksLikePatch(parsed)) return compilePatches([parsed as Record<string, unknown>])
    return asSpec(parsed)
  }
  const patches: Record<string, unknown>[] = []
  for (const [index, line] of lines.entries()) {
    let parsed: unknown
    try {
      parsed = JSON.parse(line)
    } catch (error) {
      throw new GenerativeUiSpecError('malformed_json', [`line ${index + 1}: ${errorText(error)}`])
    }
    if (!looksLikePatch(parsed)) {
      throw new GenerativeUiSpecError('malformed_json', [
        `line ${index + 1}: expected a JSON Patch operation with "op" and "path"`
      ])
    }
    patches.push(parsed as Record<string, unknown>)
  }
  return compilePatches(patches)
}

function looksLikePatch(value: unknown): boolean {
  if (typeof value === 'string') {
    try {
      value = JSON.parse(value)
    } catch {
      return false
    }
  }
  return (
    !!value
    && typeof value === 'object'
    && !Array.isArray(value)
    && typeof (value as Record<string, unknown>).op === 'string'
    && typeof (value as Record<string, unknown>).path === 'string'
  )
}

function compilePatches(patches: Record<string, unknown>[]): Spec {
  let compiled: Record<string, unknown>
  try {
    compiled = compileSpecStream(patches.map(patch => JSON.stringify(patch)).join('\n'))
  } catch (error) {
    throw new GenerativeUiSpecError('malformed_json', [errorText(error)])
  }
  return asSpec(compiled)
}

function asSpec(value: unknown): Spec {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new GenerativeUiSpecError('invalid_spec', ['spec must be a JSON object with "root" and "elements"'])
  }
  const record = value as Record<string, unknown>
  const issues: string[] = []
  if (typeof record.root !== 'string' || !record.root) issues.push('"root" must be a non-empty string')
  if (!record.elements || typeof record.elements !== 'object' || Array.isArray(record.elements)) {
    issues.push('"elements" must be an object keyed by element id')
  }
  if (issues.length > 0) throw new GenerativeUiSpecError('invalid_spec', issues)
  return record as unknown as Spec
}

/** Validates a block against the catalog and limits and returns the Chat SDK card. */
export function buildGenerativeUiCard(block: string, limits: GenerativeUiLimits = DEFAULT_GENERATIVE_UI_LIMITS): CardElement {
  const spec = parseGenerativeUiSpec(block)
  try {
    return toCard(spec, { components: generativeUiComponents, limits })
  } catch (error) {
    if (error instanceof CardSpecError) throw new GenerativeUiSpecError('invalid_spec', error.issues)
    throw new GenerativeUiSpecError('invalid_spec', [errorText(error)])
  }
}

/** The block as plain markdown code so a rejected card loses nothing. */
export function generativeUiBlockAsCode(block: string): string {
  return `\`\`\`json\n${block.replace(/\n$/, '')}\n\`\`\``
}

export type GenerativeUiCapture = {
  /** Bodies of the json-render blocks seen in the stream, in order. */
  blocks: string[]
}

const OPEN_FENCE_LINE = /^```[ \t]*json-render[ \t]*\n/
const OPEN_FENCE_PREFIX = '```json-render'
const CLOSE_FENCE_LINE = /^```[ \t]*(\n|$)/m

/**
 * Removes the first `json-render` block from a live markdown chunk stream
 * while it streams, recording its body in `capture`. Text is held back only
 * while it could still be the start of a fence line or while a block is open,
 * so prose keeps streaming and the block never reaches Slack as text. A block
 * left open when the stream ends is flushed verbatim. Later blocks pass
 * through untouched (they render as code) and are recorded so the caller can
 * report them.
 */
export async function* stripGenerativeUiBlocks(
  stream: AsyncIterable<ChatSDKStreamChunk>,
  capture: GenerativeUiCapture
): AsyncIterable<ChatSDKStreamChunk> {
  let pending = ''
  let atLineStart = true
  let inBlock = false
  let stripping = false
  // Last two characters sent downstream, to drop the blank line a removed
  // block would otherwise leave between the paragraphs around it.
  let emittedTail = ''
  let afterStrippedBlock = false

  const emit = function (text: string): ChatSDKStreamChunk {
    emittedTail = (emittedTail + text).slice(-2)
    afterStrippedBlock = false
    return { type: 'markdown_text', text }
  }

  const drain = function* (final: boolean): Iterable<ChatSDKStreamChunk> {
    while (pending) {
      if (afterStrippedBlock && pending.startsWith('\n') && emittedTail === '\n\n') {
        pending = pending.slice(1)
        afterStrippedBlock = false
        continue
      }
      if (inBlock) {
        const close = CLOSE_FENCE_LINE.exec(pending)
        // Wait for the closing fence, and for its line end so a line that
        // merely starts with three backticks is not taken for one.
        if (!close || (close[1] === '' && !final)) break
        const body = pending.slice(0, close.index).replace(/\n$/, '')
        const raw = pending.slice(0, close.index + close[0].length)
        pending = pending.slice(close.index + close[0].length)
        inBlock = false
        capture.blocks.push(body)
        if (stripping) afterStrippedBlock = true
        else yield emit(`${OPEN_FENCE_PREFIX}\n${raw}`)
        stripping = false
        atLineStart = true
        continue
      }
      if (atLineStart) {
        const open = OPEN_FENCE_LINE.exec(pending)
        if (open) {
          pending = pending.slice(open[0].length)
          inBlock = true
          stripping = capture.blocks.length === 0
          continue
        }
      }
      const newline = pending.indexOf('\n')
      if (newline === -1) {
        if (atLineStart && !final && isFenceLinePrefix(pending)) break
        yield emit(pending)
        pending = ''
        atLineStart = false
        break
      }
      yield emit(pending.slice(0, newline + 1))
      pending = pending.slice(newline + 1)
      atLineStart = true
    }
  }

  for await (const chunk of stream) {
    if (chunk.type !== 'markdown_text') {
      yield chunk
      continue
    }
    pending += chunk.text
    yield* drain(false)
  }
  // A closing fence at the very end of the answer has no newline after it.
  yield* drain(true)
  if (inBlock) {
    // The stream ended inside a block: deliver it as text so nothing is lost.
    inBlock = false
    stripping = false
    const text = `${OPEN_FENCE_PREFIX}\n${pending}`
    pending = ''
    yield emit(text)
  }
}

function isFenceLinePrefix(text: string): boolean {
  const compact = text.replace(/[ \t]+/g, '')
  return OPEN_FENCE_PREFIX.startsWith(compact) || compact.startsWith(OPEN_FENCE_PREFIX)
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
