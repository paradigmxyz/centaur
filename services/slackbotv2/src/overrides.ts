/**
 * Inline message directives, restored from the v1 slackbot:
 *   --claude | --claude-code | --amp | --codex   pick the harness for the thread
 *   --model <name> (or --model=<name>)           pick the model within that harness
 *   --fable | --opus | --sonnet | --haiku        model shortcuts (imply claude-code)
 *
 * Flags are stripped from the text before it reaches the agent. The harness
 * applies at session creation — an explicit harness flag on a thread pinned to
 * another harness restarts the thread on the requested one. The model applies
 * per turn via the blocks-protocol `model` field; `--model` accepts either a
 * full model id (claude-sonnet-4-6, gpt-5.2, …), an amp mode (deep/fast), or a
 * Claude alias (fable/opus/sonnet/haiku) which expands to the full id.
 */

export type MessageOverrides = {
  cleanedText: string
  harnessType?: string
  model?: string
}

// Flag name -> HarnessType wire value (serde lowercase of the Rust enum).
const HARNESS_FLAGS: Record<string, string> = {
  amp: 'amp',
  claude: 'claudecode',
  'claude-code': 'claudecode',
  claudecode: 'claudecode',
  codex: 'codex'
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

const MODEL_FLAG_PATTERN = /(?:^|\s)--model[=\s]+([A-Za-z0-9._/-]+)(?=\s|$)/i

export function extractMessageOverrides(text: string): MessageOverrides {
  let cleaned = text
  let harnessType: string | undefined
  let model: string | undefined

  const modelMatch = MODEL_FLAG_PATTERN.exec(cleaned)
  if (modelMatch) {
    const value = modelMatch[1]!
    model = CLAUDE_MODEL_ALIASES[value.toLowerCase()] ?? value
    cleaned = stripMatch(cleaned, modelMatch)
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

  return {
    cleanedText: cleaned === text ? text : cleaned.trim(),
    harnessType,
    model
  }
}

function flagPattern(flag: string): RegExp {
  return new RegExp(`(?:^|\\s)--${flag.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?=\\s|$)`, 'i')
}

function stripMatch(text: string, match: RegExpExecArray): string {
  return `${text.slice(0, match.index)}${text.slice(match.index + match[0].length)}`
}
