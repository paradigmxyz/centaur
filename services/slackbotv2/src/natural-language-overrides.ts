import { validateStrategyOverrides } from './overrides'
import type { HarnessOverrides } from './overrides'

type OverrideField = 'harness' | 'model' | 'provider'
type Alias = { field: OverrideField; value: string }

const ALIASES: Record<string, Alias> = {
  amp: { field: 'harness', value: 'amp' },
  bedrock: { field: 'provider', value: 'amazon-bedrock' },
  claude: { field: 'harness', value: 'claudecode' },
  claudecode: { field: 'harness', value: 'claudecode' },
  codex: { field: 'harness', value: 'codex' },
  fable: { field: 'model', value: 'claude-fable-5' },
  haiku: { field: 'model', value: 'claude-haiku-4-5' },
  luna: { field: 'model', value: 'gpt-5.6-luna' },
  meta: { field: 'provider', value: 'responses' },
  nanocodex: { field: 'harness', value: 'nanocodex' },
  openrouter: { field: 'provider', value: 'openrouter' },
  opus: { field: 'model', value: 'claude-opus-4-8' },
  sol: { field: 'model', value: 'gpt-5.6-sol' },
  sonnet: { field: 'model', value: 'claude-sonnet-4-6' },
  terra: { field: 'model', value: 'gpt-5.6-terra' }
}

const CLAUSE_PATTERN = new RegExp(
  String.raw`\b(?:use|using|select|choose|pick|switch(?:ing)?(?:\s+me)?\s+to|run(?:ning)?(?:\s+this)?\s+(?:with|on))\b([^.!?\n]{0,120})`,
  'gi'
)
const TOKEN_PATTERN = /[a-z0-9]+(?:[.-][a-z0-9]+)*/g
const REASONING_PATTERN =
  /\b(none|minimal|low|medium|high|xhigh|max)\s+(?:reasoning|effort)\b|\b(?:reasoning|effort)\s+(?:to\s+)?(none|minimal|low|medium|high|xhigh|max)\b/i
const AMP_MODEL_PATTERN = /\b(deep|fast)\s+(?:model|mode)\b/i

/**
 * Resolve common natural-language model selections without a network call.
 * Matching is limited to explicit selection clauses, and edit-distance
 * matching is reserved for names of at least five characters to avoid
 * treating ordinary short words as selectors.
 */
export function extractNaturalLanguageOverrides(text: string): HarnessOverrides | undefined {
  const raw: Record<OverrideField | 'reasoning', string | undefined> = {
    harness: undefined,
    model: undefined,
    provider: undefined,
    reasoning: undefined
  }
  let matched = false

  for (const clauseMatch of text.matchAll(CLAUSE_PATTERN)) {
    const clause = clauseMatch[1] ?? ''
    const tokens = clause.toLowerCase().match(TOKEN_PATTERN) ?? []
    for (const [index, token] of tokens.entries()) {
      const alias = resolveAlias(token)
      if (!alias || !isSelectorPosition(tokens, index)) continue
      raw[alias.field] = alias.value
      matched = true
    }

    const reasoningMatch = REASONING_PATTERN.exec(clause)
    const reasoning = reasoningMatch?.[1] ?? reasoningMatch?.[2]
    if (reasoning) {
      raw.reasoning = reasoning.toLowerCase()
      matched = true
    }

    const ampModelMatch = AMP_MODEL_PATTERN.exec(clause)
    if (ampModelMatch) {
      raw.model = ampModelMatch[1]!.toLowerCase()
      matched = true
    }
  }

  if (!matched) return undefined
  const overrides = validateStrategyOverrides(raw)
  return Object.values(overrides).some(value => value !== undefined) ? overrides : undefined
}

function isSelectorPosition(tokens: string[], index: number): boolean {
  if (index <= 2) return true
  return ['agent', 'harness', 'mode', 'model', 'provider'].includes(tokens[index + 1] ?? '')
}

function resolveAlias(token: string): Alias | undefined {
  const exact = ALIASES[token]
  if (exact) return exact
  if (token.length < 5) return undefined

  const matches = Object.entries(ALIASES).filter(
    ([name]) => name.length >= 5 && editDistanceAtMostOne(token, name)
  )
  return matches.length === 1 ? matches[0]![1] : undefined
}

function editDistanceAtMostOne(left: string, right: string): boolean {
  if (Math.abs(left.length - right.length) > 1) return false
  if (left === right) return true

  if (left.length === right.length) {
    let differences = 0
    for (let index = 0; index < left.length; index += 1) {
      if (left[index] !== right[index] && ++differences > 1) return false
    }
    return true
  }

  const [shorter, longer] = left.length < right.length ? [left, right] : [right, left]
  let shortIndex = 0
  let longIndex = 0
  let skipped = false
  while (shortIndex < shorter.length && longIndex < longer.length) {
    if (shorter[shortIndex] === longer[longIndex]) {
      shortIndex += 1
      longIndex += 1
      continue
    }
    if (skipped) return false
    skipped = true
    longIndex += 1
  }
  return true
}
