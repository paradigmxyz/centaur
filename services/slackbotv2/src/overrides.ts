/**
 * Inline message directives, restored from the v1 slackbot:
 *   --claude | --claude-code | --amp | --codex | --nanocodex
 *                                                  pick the harness for the thread
 *   --bedrock                                    codex via the AWS Bedrock provider
 *   --meta                                       codex via Meta AI direct
 *   --model <name> (or --model=<name>)           pick the model within that harness
 *   -rsn <effort> (or -rsn=<effort>)             per-turn reasoning effort (codex)
 *   --fable | --opus | --sonnet | --haiku        model shortcuts (imply claude-code)
 *
 * Flags are stripped from the text before it reaches the agent. The harness
 * applies at session creation — an explicit harness flag on a thread pinned to
 * another harness restarts the thread on the requested one. Harness/model/provider
 * choices are sticky at the Slack thread level: the last flag wins for later
 * turns in the same thread. `--model` accepts either a full model id
 * (claude-sonnet-4-6, gpt-5.2, ...), an amp mode (deep/fast), or a Claude alias
 * (fable/opus/sonnet/haiku) which expands to the full id. Reasoning effort only
 * affects the codex harness (it maps to codex's `turn/start` `effort`) and stays
 * per-turn; other harnesses ignore it. The provider rides the blocks-protocol
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

// Flag name -> HarnessType wire value (serde lowercase of the Rust enum).
const HARNESS_FLAGS: Record<string, string> = {
  amp: 'amp',
  claude: 'claudecode',
  'claude-code': 'claudecode',
  claudecode: 'claudecode',
  codex: 'codex',
  nanocodex: 'nanocodex'
}

// Provider flags select a model provider within the codex harness (and imply
// it). Bedrock rides codex's built-in `amazon-bedrock` provider, whose wire
// value is passed through as the blocks-protocol `provider` field.
const PROVIDER_FLAGS: Record<string, { provider: string; harnessType: string }> = {
  bedrock: { provider: 'amazon-bedrock', harnessType: 'codex' },
  meta: { provider: 'responses', harnessType: 'codex' }
}

// Claude model aliases, usable both as bare flags (--opus) and as --model
// values (--model opus). Bare-flag form also implies the claude-code harness.
const CLAUDE_MODEL_ALIASES: Record<string, string> = {
  fable: 'claude-fable-5',
  haiku: 'claude-haiku-4-5',
  opus: 'claude-opus-4-8',
  sonnet: 'claude-sonnet-4-6'
}

const MODEL_SHORTCUTS: Record<string, { harnessType: string; model: string }> =
  Object.fromEntries(
    Object.entries(CLAUDE_MODEL_ALIASES).map(([alias, model]) => [
      alias,
      { harnessType: 'claudecode', model }
    ])
  )

const STRATEGY_HARNESSES = new Set(['amp', 'claudecode', 'codex', 'nanocodex'])
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

// Single dash by design: a short per-turn knob (`-rsn high`), so it can't reuse
// the `--`-prefixed flagPattern() helper. Value-capturing like --model.
const REASONING_FLAG_PATTERN = new RegExp(
  String.raw`(?:^|\s)-rsn${MODEL_VALUE_SEPARATOR}([A-Za-z-]+)${FLAG_VALUE_BOUNDARY}`,
  'i'
)

// Codex reasoning efforts (turn/start `effort`), plus convenience aliases.
const REASONING_EFFORTS: Record<string, string> = {
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

export function extractMessageOverrides(text: string): MessageOverrides {
  let cleaned = text
  let harnessType: string | undefined
  let model: string | undefined
  let provider: string | undefined
  let reasoning: string | undefined

  const modelMatch = MODEL_FLAG_PATTERN.exec(cleaned)
  if (modelMatch) {
    const value = modelMatch[1]!
    model = CLAUDE_MODEL_ALIASES[value.toLowerCase()] ?? value
    cleaned = stripMatch(cleaned, modelMatch)
  }

  const reasoningMatch = REASONING_FLAG_PATTERN.exec(cleaned)
  if (reasoningMatch) {
    const normalized = REASONING_EFFORTS[reasoningMatch[1]!.toLowerCase()]
    if (normalized) {
      reasoning = normalized
      cleaned = stripMatch(cleaned, reasoningMatch)
    }
  }

  for (const [flag, harness] of Object.entries(HARNESS_FLAGS)) {
    const match = flagPattern(flag).exec(cleaned)
    if (!match) continue
    harnessType = harness
    cleaned = stripMatch(cleaned, match)
  }

  for (const [flag, shortcut] of Object.entries(MODEL_SHORTCUTS)) {
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
  } | null | undefined
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
    const modelHarness = STRATEGY_MODEL_HARNESSES[modelRaw.toLowerCase()]
    if (!modelHarness) return {}
    if (harnessType && harnessType !== modelHarness) return {}
    model = modelRaw.toLowerCase()
    harnessType = modelHarness
  }

  const reasoningRaw = cleanString(raw.reasoning)
  if (reasoningRaw) {
    const normalized = reasoningRaw.toLowerCase()
    if (!STRATEGY_REASONING_EFFORTS.has(normalized)) return {}
    reasoning = harnessType === undefined || harnessType === 'codex' ? normalized : undefined
  }

  return { harnessType, model, provider, reasoning }
}

/**
 * Object-shaped counterpart to {@link extractMessageOverrides}: normalizes a
 * `{ harness, model, provider, reasoning }` config through the same vocabulary
 * as the flag parser (harness/provider/model aliases; a provider implies its
 * harness, like `--bedrock`). Fields are independent; unrecognized harness /
 * provider / reasoning values are reported via `onError` and dropped.
 */
export function normalizeHarnessOverrides(
  raw: { harness?: unknown; model?: unknown; provider?: unknown; reasoning?: unknown },
  onError?: (message: string) => void
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
    const mapping = PROVIDER_FLAGS[providerRaw.toLowerCase()]
    if (mapping) {
      provider = mapping.provider
      harnessType ??= mapping.harnessType // a provider implies its harness, like --bedrock
    } else {
      onError?.(`unknown provider "${providerRaw}"`)
    }
  }

  const modelRaw = cleanString(raw.model)
  if (modelRaw) model = CLAUDE_MODEL_ALIASES[modelRaw.toLowerCase()] ?? modelRaw

  const reasoningRaw = cleanString(raw.reasoning)
  if (reasoningRaw) {
    reasoning = REASONING_EFFORTS[reasoningRaw.toLowerCase()]
    if (!reasoning) onError?.(`unknown reasoning effort "${reasoningRaw}"`)
  }

  return { harnessType, model, provider, reasoning }
}

function cleanString(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  return trimmed === '' ? undefined : trimmed
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
