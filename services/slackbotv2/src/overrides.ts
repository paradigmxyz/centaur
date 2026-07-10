/**
 * Inline message directives, restored from the v1 slackbot:
 *   --claude | --claude-code | --amp | --codex   pick the harness for the thread
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

export type MessageOverrides = {
  cleanedText: string
  harnessType?: string
  model?: string
  provider?: string
  reasoning?: string
}

// Flag name -> HarnessType wire value (serde lowercase of the Rust enum).
const HARNESS_FLAGS: Record<string, string> = {
  amp: 'amp',
  claude: 'claudecode',
  'claude-code': 'claudecode',
  claudecode: 'claudecode',
  codex: 'codex'
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
