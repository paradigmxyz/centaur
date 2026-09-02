/**
 * Inline message directives, restored from the v1 slackbot:
 *   --claude | --claude-code | --amp | --codex | --nanocodex
 *                                                  pick the harness for the thread
 *   --bedrock                                    codex via the AWS Bedrock provider
 *   --meta                                       codex via Meta AI direct
 *   --provider <name>                            codex via a configured provider
 *   --model <name> (or --model=<name>)           pick the model within that harness
 *   -rsn <effort> (or -rsn=<effort>)             per-turn reasoning effort (codex/nanocodex)
 *   --fable | --opus | --sonnet | --haiku        model shortcuts (imply claude-code)
 *
 * Flags are stripped from the text before it reaches the agent. The harness
 * applies at session creation — an explicit harness flag on a thread pinned to
 * another harness restarts the thread on the requested one. Harness/model/provider
 * choices are sticky at the Slack thread level: the last flag wins for later
 * turns in the same thread. `--model` accepts either a full model id
 * (claude-sonnet-4-6, gpt-5.2, ...), an amp mode (deep/fast), or a Claude alias
 * (fable/opus/sonnet/haiku) which expands to the full id. Reasoning effort only
 * affects the codex-compatible harnesses and stays per-turn; other harnesses
 * ignore it. The provider rides the blocks-protocol
 * `provider` field and is fixed when the codex thread starts. Provider
 * shortcuts imply the codex harness.
 */

/**
 * A resolved bundle of harness knobs (harness + model/provider/reasoning), all
 * optional. Shared by the inline flag parser and per-channel defaults so both
 * speak the same vocabulary.
 */
export type HarnessOverrides = {
  harnessType?: string
  model?: string
  provider?: string
  reasoning?: string
}

export type MessageOverrides = HarnessOverrides & {
  cleanedText: string
}

export type ModelAlias = {
  harnessType: string
  model: string
}

export type OverrideAliases = {
  model: Record<string, ModelAlias>
  reasoning: Record<string, string>
}

// Flag name -> HarnessType wire value (serde lowercase of the Rust enum).
const HARNESS_FLAGS: Record<string, string> = {
  amp: 'amp',
  claude: 'claudecode',
  'claude-code': 'claudecode',
  claudecode: 'claudecode',
  codex: 'codex',
  hermes: 'hermes',
  nanocodex: 'nanocodex'
}

// Provider flags select a model provider within the codex harness (and imply
// it). Bedrock rides codex's built-in `amazon-bedrock` provider, whose wire
// value is passed through as the blocks-protocol `provider` field.
type ProviderMapping = { provider: string; harnessType: string; model?: string }

const PROVIDER_FLAGS: Record<string, ProviderMapping> = {
  bedrock: { provider: 'amazon-bedrock', harnessType: 'codex' },
  meta: { provider: 'responses', harnessType: 'codex' }
}

// Built-in model aliases, usable both as bare flags (--opus/--sol) and as
// --model values (--model opus). Bare-flag form also implies the configured
// compatible harness.
const DEFAULT_MODEL_ALIASES: Record<string, ModelAlias> = {
  '5.4': { harnessType: 'codex', model: 'gpt-5.4' },
  '5.5': { harnessType: 'codex', model: 'gpt-5.5' },
  fable: { harnessType: 'claudecode', model: 'claude-fable-5' },
  haiku: { harnessType: 'claudecode', model: 'claude-haiku-4-5' },
  luna: { harnessType: 'codex', model: 'gpt-5.6-luna' },
  opus: { harnessType: 'claudecode', model: 'claude-opus-4-8' },
  sol: { harnessType: 'codex', model: 'gpt-5.6-sol' },
  sonnet: { harnessType: 'claudecode', model: 'claude-sonnet-4-6' },
  terra: { harnessType: 'codex', model: 'gpt-5.6-terra' }
}

const STRATEGY_HARNESSES = new Set(['amp', 'claudecode', 'codex', 'hermes', 'nanocodex'])
const STRATEGY_PROVIDERS = new Set(['amazon-bedrock', 'openrouter', 'responses'])
const STRATEGY_REASONING_EFFORTS = new Set([
  'none',
  'minimal',
  'low',
  'medium',
  'high',
  'xhigh',
  'max'
])

const STRATEGY_MODEL_HARNESSES: Record<string, string> = {
  'claude-fable-5': 'claudecode',
  'claude-haiku-4-5': 'claudecode',
  'claude-opus-4-7': 'claudecode',
  'claude-opus-4-8': 'claudecode',
  'claude-opus-5': 'claudecode',
  'claude-opus-5-fast': 'claudecode',
  'claude-sonnet-4-6': 'claudecode',
  'claude-sonnet-5': 'claudecode',
  deep: 'amp',
  fast: 'amp',
  'gpt-5.4': 'codex',
  'gpt-5.4-mini': 'codex',
  'gpt-5.4-nano': 'codex',
  'gpt-5.4-pro': 'codex',
  'gpt-5.5': 'codex',
  'gpt-5.5-pro': 'codex',
  'gpt-5.6-luna': 'codex',
  'gpt-5.6-sol': 'codex',
  'gpt-5.6-terra': 'codex'
}

// Values are one horizontal-whitespace-delimited token; a newline after the
// value starts the user's prompt, not part of the model/reasoning value.
const MODEL_VALUE_SEPARATOR = String.raw`(?:[^\S\r\n]*=[^\S\r\n]*|[^\S\r\n]+)`
const FLAG_VALUE_BOUNDARY = String.raw`(?=[^\S\r\n]|\r?\n|\r|<br\s*/?>|$)`

const MODEL_FLAG_PATTERN = new RegExp(
  String.raw`(?:^|\s)--model${MODEL_VALUE_SEPARATOR}([A-Za-z0-9._/-]+)${FLAG_VALUE_BOUNDARY}`,
  'i'
)

const PROVIDER_FLAG_PATTERN = new RegExp(
  String.raw`(?:^|\s)--provider${MODEL_VALUE_SEPARATOR}([A-Za-z][A-Za-z0-9_-]*)${FLAG_VALUE_BOUNDARY}`,
  'i'
)

// Single dash by design: a short per-turn knob (`-rsn high`), so it can't reuse
// the `--`-prefixed flagPattern() helper. Value-capturing like --model.
const REASONING_FLAG_PATTERN = new RegExp(
  String.raw`(?:^|\s)-rsn${MODEL_VALUE_SEPARATOR}([A-Za-z0-9._-]+)${FLAG_VALUE_BOUNDARY}`,
  'i'
)

// Codex reasoning efforts (turn/start `effort`), plus convenience aliases.
const DEFAULT_REASONING_ALIASES: Record<string, string> = {
  none: 'none',
  minimal: 'minimal',
  min: 'minimal',
  low: 'low',
  medium: 'medium',
  med: 'medium',
  high: 'high',
  hi: 'high',
  xhigh: 'xhigh',
  xhi: 'xhigh',
  'x-high': 'xhigh',
  max: 'max'
}

export const DEFAULT_OVERRIDE_ALIASES: OverrideAliases = {
  model: DEFAULT_MODEL_ALIASES,
  reasoning: DEFAULT_REASONING_ALIASES
}

export function mergeOverrideAliases(
  custom: Partial<OverrideAliases> | undefined
): OverrideAliases {
  return {
    model: { ...DEFAULT_MODEL_ALIASES, ...custom?.model },
    reasoning: { ...DEFAULT_REASONING_ALIASES, ...custom?.reasoning }
  }
}

export function parseOverrideAliases(
  modelAliasesRaw: string | undefined,
  reasoningAliasesRaw: string | undefined,
  onError?: (message: string) => void
): OverrideAliases {
  const model = parseAliasObject(modelAliasesRaw, 'model', onError, value => {
    if (!isPlainObject(value)) return undefined
    const model = cleanString(value.model)
    const harnessRaw = cleanString(value.harness)
    const harnessType = harnessRaw ? HARNESS_FLAGS[harnessRaw.toLowerCase()] : undefined
    if (!model || !harnessType) return undefined
    return { harnessType, model }
  })
  const reasoning = parseAliasObject(reasoningAliasesRaw, 'reasoning', onError, value => {
    const effort = cleanString(value)?.toLowerCase()
    return effort && STRATEGY_REASONING_EFFORTS.has(effort) ? effort : undefined
  })
  return mergeOverrideAliases({ model, reasoning })
}

export function extractMessageOverrides(
  text: string,
  aliases: OverrideAliases = DEFAULT_OVERRIDE_ALIASES
): MessageOverrides {
  let cleaned = text
  let harnessType: string | undefined
  let model: string | undefined
  let provider: string | undefined
  let reasoning: string | undefined

  const modelMatch = MODEL_FLAG_PATTERN.exec(cleaned)
  if (modelMatch) {
    const value = modelMatch[1]!
    model = aliases.model[value.toLowerCase()]?.model ?? value
    cleaned = stripMatch(cleaned, modelMatch)
  }

  const reasoningMatch = REASONING_FLAG_PATTERN.exec(cleaned)
  if (reasoningMatch) {
    const normalized = aliases.reasoning[reasoningMatch[1]!.toLowerCase()]
    if (normalized) {
      reasoning = normalized
      cleaned = stripMatch(cleaned, reasoningMatch)
    }
  }

  const providerMatch = PROVIDER_FLAG_PATTERN.exec(cleaned)
  if (providerMatch) {
    const mapping = providerMapping(providerMatch[1]!)!
    provider = mapping.provider
    harnessType ??= mapping.harnessType
    model ??= mapping.model
    cleaned = stripMatch(cleaned, providerMatch)
  }

  for (const [flag, harness] of Object.entries(HARNESS_FLAGS)) {
    const match = flagPattern(flag).exec(cleaned)
    if (!match) continue
    harnessType = harness
    cleaned = stripMatch(cleaned, match)
  }

  for (const [flag, shortcut] of Object.entries(aliases.model)) {
    const match = flagPattern(flag).exec(cleaned)
    if (!match) continue
    model ??= shortcut.model
    harnessType ??= shortcut.harnessType
    cleaned = stripMatch(cleaned, match)
  }

  for (const [flag, mapping] of Object.entries(PROVIDER_FLAGS)) {
    const match = flagPattern(flag).exec(cleaned)
    if (!match) continue
    provider ??= mapping.provider
    harnessType ??= mapping.harnessType
    model ??= mapping.model
    cleaned = stripMatch(cleaned, match)
  }

  return {
    cleanedText: cleaned === text ? text : cleaned.trim(),
    harnessType,
    model,
    provider,
    reasoning
  }
}

export function validateStrategyOverrides(
  raw: {
    harness?: unknown
    model?: unknown
    provider?: unknown
    reasoning?: unknown
  } | null | undefined,
  aliases: OverrideAliases = DEFAULT_OVERRIDE_ALIASES
): HarnessOverrides {
  if (!raw || typeof raw !== 'object') return {}
  let harnessType: string | undefined
  let model: string | undefined
  let provider: string | undefined
  let reasoning: string | undefined

  const harnessRaw = cleanString(raw.harness)
  if (harnessRaw) {
    const normalized = harnessRaw.toLowerCase()
    if (!STRATEGY_HARNESSES.has(normalized)) return {}
    harnessType = normalized
  }

  const providerRaw = cleanString(raw.provider)
  if (providerRaw) {
    const normalized = providerRaw.toLowerCase()
    if (!STRATEGY_PROVIDERS.has(normalized)) return {}
    provider = normalized
    if (harnessType && harnessType !== 'codex') return {}
    harnessType = 'codex'
  }

  const modelRaw = cleanString(raw.model)
  if (modelRaw) {
    const resolved = resolveStrategyModel(modelRaw, aliases)
    if (!resolved) return {}
    if (harnessType && harnessType !== resolved.harnessType) return {}
    model = resolved.model
    harnessType = resolved.harnessType
  }

  const reasoningRaw = cleanString(raw.reasoning)
  if (reasoningRaw) {
    const normalized = aliases.reasoning[reasoningRaw.toLowerCase()]
    if (!normalized) return {}
    reasoning =
      harnessType === undefined || harnessType === 'codex' || harnessType === 'nanocodex'
        ? normalized
        : undefined
  }

  return { harnessType, model, provider, reasoning }
}

/**
 * Object-shaped counterpart to {@link extractMessageOverrides}: normalizes a
 * `{ harness, model, provider, reasoning }` config through the same vocabulary
 * as the flag parser (harness/provider/model aliases; a provider implies its
 * harness, like `--bedrock`). Fields are independent; unrecognized harness /
 * reasoning values and malformed provider ids are reported via `onError` and
 * dropped.
 */
export function normalizeHarnessOverrides(
  raw: { harness?: unknown; model?: unknown; provider?: unknown; reasoning?: unknown },
  onError?: (message: string) => void,
  aliases: OverrideAliases = DEFAULT_OVERRIDE_ALIASES
): HarnessOverrides {
  let harnessType: string | undefined
  let model: string | undefined
  let provider: string | undefined
  let reasoning: string | undefined

  const harnessRaw = cleanString(raw.harness)
  if (harnessRaw) {
    harnessType = HARNESS_FLAGS[harnessRaw.toLowerCase()]
    if (!harnessType) onError?.(`unknown harness "${harnessRaw}"`)
  }

  const providerRaw = cleanString(raw.provider)
  if (providerRaw) {
    const mapping = providerMapping(providerRaw)
    if (mapping) {
      provider = mapping.provider
      harnessType ??= mapping.harnessType // a provider implies its harness, like --bedrock
      model ??= mapping.model
    } else {
      onError?.(`invalid provider id "${providerRaw}"`)
    }
  }

  const modelRaw = cleanString(raw.model)
  if (modelRaw) model = aliases.model[modelRaw.toLowerCase()]?.model ?? modelRaw

  const reasoningRaw = cleanString(raw.reasoning)
  if (reasoningRaw) {
    reasoning = aliases.reasoning[reasoningRaw.toLowerCase()]
    if (!reasoning) onError?.(`unknown reasoning effort "${reasoningRaw}"`)
  }

  return { harnessType, model, provider, reasoning }
}

function cleanString(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  return trimmed === '' ? undefined : trimmed
}

function resolveStrategyModel(value: string, aliases: OverrideAliases): ModelAlias | undefined {
  const normalized = value.toLowerCase()
  const alias = aliases.model[normalized]
  if (alias) return alias
  const configuredModel = Object.values(aliases.model).find(
    candidate => candidate.model.toLowerCase() === normalized
  )
  if (configuredModel) return configuredModel
  const harnessType = STRATEGY_MODEL_HARNESSES[normalized]
  return harnessType ? { harnessType, model: normalized } : undefined
}

function parseAliasObject<T>(
  raw: string | undefined,
  kind: string,
  onError: ((message: string) => void) | undefined,
  parseValue: (value: unknown) => T | undefined
): Record<string, T> {
  if (!raw?.trim()) return {}
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch (error) {
    onError?.(`${kind} aliases: invalid JSON: ${error instanceof Error ? error.message : String(error)}`)
    return {}
  }
  if (!isPlainObject(parsed)) {
    onError?.(`${kind} aliases: expected an object keyed by alias`)
    return {}
  }
  const result: Record<string, T> = {}
  for (const [rawAlias, rawValue] of Object.entries(parsed)) {
    const alias = rawAlias.trim().toLowerCase()
    const value = parseValue(rawValue)
    if (!alias || !/^[a-z0-9._-]+$/.test(alias) || !value) {
      onError?.(`${kind} aliases: invalid alias entry "${rawAlias}"`)
      continue
    }
    result[alias] = value
  }
  return result
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function providerMapping(value: string): ProviderMapping | undefined {
  const provider = value.toLowerCase()
  if (!/^[a-z][a-z0-9_-]*$/.test(provider)) return undefined
  return (
    PROVIDER_FLAGS[provider] ?? {
      provider,
      harnessType: 'codex',
      model: customProviderDefaultModel(provider)
    }
  )
}

function customProviderDefaultModel(provider: string): string | undefined {
  const raw = process.env.CODEX_CUSTOM_PROVIDERS
  if (!raw) return undefined
  try {
    const config = JSON.parse(raw)?.[provider]
    const model = config?.defaultModel
    return typeof model === 'string' && model.trim() ? model.trim() : undefined
  } catch {
    return undefined
  }
}

function flagPattern(flag: string): RegExp {
  return new RegExp(`(?:^|\\s)--${flag.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?=\\s|$)`, 'i')
}

function stripMatch(text: string, match: RegExpExecArray): string {
  const before = text.slice(0, match.index)
  const after = text
    .slice(match.index + match[0].length)
    .replace(/^(?:(?:\r\n?|\n)+|<br\s*\/?>)+/i, '')
  const separator =
    before && after && !/\s$/.test(before) && !/^\s/.test(after) ? ' ' : ''
  return `${before}${separator}${after}`
}
