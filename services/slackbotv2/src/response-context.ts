/**
 * Slack-only response metadata context line.
 *
 * Slackbotv2 appends a Block Kit `context` block according to the configured
 * metadata mode. The block is passed to the chat adapter via
 * `StreamOptions.stopBlocks`, which the adapter forwards to Slack's
 * `chat.stopStream` `blocks` argument. This keeps the rendering Slack-only and
 * out of the shared `@centaur/rendering` package used by Discord/Teams.
 */

import claudeSettings from '../../../harness/claude/settings.json'
import codexConfig from '../../../harness/codex/config.toml'

const HARNESS_DISPLAY_NAMES: Record<string, string> = {
  amp: 'Amp',
  claudecode: 'Claude Code',
  codex: 'Codex',
  nanocodex: 'Nanocodex'
}

const REASONING_DISPLAY_NAMES: Record<string, string> = {
  none: 'None',
  minimal: 'Minimal',
  low: 'Low',
  medium: 'Medium',
  high: 'High',
  xhigh: 'XHigh',
  max: 'Max',
  ultra: 'Ultra'
}

const STANDARD_CODEX_REASONING_EFFORTS = new Set([
  'none',
  'low',
  'medium',
  'high',
  'xhigh'
])
const PRO_CODEX_REASONING_EFFORTS = new Set(['medium', 'high', 'xhigh'])
const CODEX_MODEL_REASONING_EFFORTS = new Set(['low', 'medium', 'high', 'xhigh'])
const GPT_5_6_REASONING_EFFORTS = new Set([
  ...STANDARD_CODEX_REASONING_EFFORTS,
  'max'
])
const GPT_6_ASTRA_REASONING_EFFORTS = new Set([
  'low',
  'medium',
  'high',
  'xhigh',
  'max',
  'ultra'
])
// Claude Code `effortLevel` values; applied per turn by the harness server.
const CLAUDE_CODE_REASONING_EFFORTS = new Set(['low', 'medium', 'high', 'xhigh', 'max'])
// Older Claude models Claude Code limits to fewer effort levels (none for
// models without effort support); newer models support every level.
const NO_REASONING_EFFORTS: ReadonlySet<string> = new Set()
const CLAUDE_NO_XHIGH_REASONING_EFFORTS = new Set(['low', 'medium', 'high', 'max'])
const CLAUDE_REASONING_EFFORTS_BY_MODEL: Record<string, ReadonlySet<string>> = {
  'claude-haiku-4-5': NO_REASONING_EFFORTS,
  'claude-opus-4': NO_REASONING_EFFORTS,
  'claude-opus-4-0': NO_REASONING_EFFORTS,
  'claude-opus-4-1': NO_REASONING_EFFORTS,
  'claude-opus-4-5': new Set(['low', 'medium', 'high']),
  'claude-opus-4-6': CLAUDE_NO_XHIGH_REASONING_EFFORTS,
  'claude-sonnet-4': NO_REASONING_EFFORTS,
  'claude-sonnet-4-0': NO_REASONING_EFFORTS,
  'claude-sonnet-4-5': NO_REASONING_EFFORTS,
  'claude-sonnet-4-6': CLAUDE_NO_XHIGH_REASONING_EFFORTS
}
const CODEX_REASONING_EFFORTS_BY_MODEL: Record<string, ReadonlySet<string>> = {
  'gpt-5.2': STANDARD_CODEX_REASONING_EFFORTS,
  'gpt-5.2-codex': CODEX_MODEL_REASONING_EFFORTS,
  'gpt-5.4': STANDARD_CODEX_REASONING_EFFORTS,
  'gpt-5.4-mini': STANDARD_CODEX_REASONING_EFFORTS,
  'gpt-5.4-nano': STANDARD_CODEX_REASONING_EFFORTS,
  'gpt-5.4-pro': PRO_CODEX_REASONING_EFFORTS,
  'gpt-5.5': STANDARD_CODEX_REASONING_EFFORTS,
  'gpt-5.5-pro': PRO_CODEX_REASONING_EFFORTS,
  'gpt-5.6-luna': GPT_5_6_REASONING_EFFORTS,
  'gpt-5.6-sol': GPT_5_6_REASONING_EFFORTS,
  'gpt-5.6-terra': GPT_5_6_REASONING_EFFORTS,
  'gpt-6-astra': GPT_6_ASTRA_REASONING_EFFORTS,
  'gpt-6-sol': GPT_5_6_REASONING_EFFORTS,
  'gpt-6-luna': GPT_5_6_REASONING_EFFORTS
}

const CODEX_CONFIG = codexConfig as {
  model?: unknown
  model_reasoning_effort?: unknown
  service_tier?: unknown
}

// Default model each harness runs when no --model/--opus/... override is set,
// read from the same harness config files the sandbox images bake in
// (harness/claude/settings.json, harness/codex/config.toml; the slackbotv2
// Dockerfile copies harness/ so these imports resolve in the image too).
// Deployers who override the sandbox model via CLAUDE_MODEL / CODEX_MODEL
// (sandbox.extraEnv) get the same values mirrored into slackbotv2 by the chart
// and passed here through SlackbotV2Options.harnessDefaultModels, which takes
// precedence. Amp has no fixed default model (deep/fast modes), so it is
// intentionally absent.
const BAKED_DEFAULT_MODELS: Record<string, string | undefined> = {
  claudecode: typeof claudeSettings.model === 'string' ? claudeSettings.model : undefined,
  codex: typeof CODEX_CONFIG.model === 'string' ? CODEX_CONFIG.model : undefined,
  nanocodex: typeof CODEX_CONFIG.model === 'string' ? CODEX_CONFIG.model : undefined
}

// Nanocodex deliberately shares Codex's default reasoning policy. Its harness
// adapter consumes the same CODEX_MODEL_REASONING_EFFORT deployment override,
// and falls back to the baked Codex effort when that override is absent.
const BAKED_DEFAULT_REASONING: Record<string, string | undefined> = {
  codex:
    typeof CODEX_CONFIG.model_reasoning_effort === 'string'
      ? CODEX_CONFIG.model_reasoning_effort
      : undefined,
  nanocodex:
    typeof CODEX_CONFIG.model_reasoning_effort === 'string'
      ? CODEX_CONFIG.model_reasoning_effort
      : undefined
}

const BAKED_DEFAULT_SERVICE_TIERS: Record<string, string | undefined> = {
  codex: typeof CODEX_CONFIG.service_tier === 'string' ? CODEX_CONFIG.service_tier : undefined
}

/** Slack mrkdwn requires `&`, `<`, `>` to be escaped in free text. */
function escapeSlackMrkdwn(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

function titleCase(value: string): string {
  return value
    .split(/[\s_-]+/)
    .filter(Boolean)
    .map(word => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ')
}

/**
 * Maps a harness wire value (codex | claudecode | amp) to a human display name.
 * Unknown harnesses fall back to a title-cased form of the raw value. Returns
 * undefined when no harness is provided.
 */
export function harnessDisplayName(harnessType: string | null | undefined): string | undefined {
  if (!harnessType) return undefined
  const key = harnessType.trim().toLowerCase()
  if (!key) return undefined
  return HARNESS_DISPLAY_NAMES[key] ?? titleCase(key)
}

/**
 * Returns the model a harness runs by default (no explicit override):
 * the deployment-configured value (CLAUDE_MODEL / CODEX_MODEL via
 * SlackbotV2Options.harnessDefaultModels, keyed by harness wire value) when
 * set, else the model pinned in this repo's harness config files. Undefined
 * for harnesses without a fixed default (amp, unknown harnesses).
 */
export function defaultModelForHarness(
  harnessType: string | null | undefined,
  configured?: Record<string, string>
): string | undefined {
  if (!harnessType) return undefined
  const key = harnessType.trim().toLowerCase()
  return configured?.[key]?.trim() || BAKED_DEFAULT_MODELS[key]
}

/** Returns the configured or baked default reasoning effort for a harness. */
export function defaultReasoningForHarness(
  harnessType: string | null | undefined,
  configured?: Record<string, string>
): string | undefined {
  if (!harnessType) return undefined
  const key = harnessType.trim().toLowerCase()
  return configured?.[key]?.trim().toLowerCase() || BAKED_DEFAULT_REASONING[key]
}

/** Returns the baked service tier for a harness that consumes Codex config.toml. */
export function defaultServiceTierForHarness(
  harnessType: string | null | undefined
): string | undefined {
  if (!harnessType) return undefined
  const key = harnessType.trim().toLowerCase()
  return BAKED_DEFAULT_SERVICE_TIERS[key]
}

/** Resolves the effort the selected harness actually runs for this turn. */
export function effectiveReasoningForHarness(
  harnessType: string | null | undefined,
  requested?: string | null,
  configured?: Record<string, string>
): string | undefined {
  const key = harnessType?.trim().toLowerCase()
  // Claude Code's default effort depends on the model, so only a requested
  // (already model-validated) effort is known.
  if (key === 'claudecode') return requested?.trim().toLowerCase() || undefined
  if (key !== 'codex' && key !== 'nanocodex') return undefined
  const reasoning = requested?.trim().toLowerCase() || defaultReasoningForHarness(key, configured)
  // Nanocodex has no distinct Minimal level; its adapter maps Minimal to Low.
  return key === 'nanocodex' && reasoning === 'minimal' ? 'low' : reasoning
}

/** Returns the requested effort only when the selected model supports it. */
export function reasoningForModel(
  harnessType: string | null | undefined,
  model: string | null | undefined,
  reasoning: string | null | undefined
): string | undefined {
  const harness = harnessType?.trim().toLowerCase()
  const selectedModel = model?.trim().toLowerCase()
  const effort = reasoning?.trim().toLowerCase()
  if (harness === 'claudecode') {
    const supported =
      Object.entries(CLAUDE_REASONING_EFFORTS_BY_MODEL).find(
        ([modelId]) => selectedModel === modelId || selectedModel?.startsWith(`${modelId}-20`)
      )?.[1] ?? CLAUDE_CODE_REASONING_EFFORTS
    return effort && supported.has(effort) ? effort : undefined
  }
  if (!selectedModel || !effort) return undefined
  if (harness !== 'codex' && harness !== 'nanocodex') return undefined
  // Nanocodex maps its compatibility-only Minimal value to Low before it
  // reaches the Responses API, so validate the effective value against the
  // selected model while preserving Minimal for the adapter to normalize.
  const effectiveEffort = harness === 'nanocodex' && effort === 'minimal' ? 'low' : effort
  if (selectedModel === 'gpt-5.6') {
    return GPT_5_6_REASONING_EFFORTS.has(effectiveEffort) ? effort : undefined
  }
  const supported = Object.entries(CODEX_REASONING_EFFORTS_BY_MODEL).find(
    ([modelId]) => selectedModel === modelId || selectedModel.startsWith(`${modelId}-20`)
  )?.[1]
  return supported?.has(effectiveEffort) ? effort : undefined
}

function reasoningDisplayName(reasoning: string | null | undefined): string | undefined {
  const key = reasoning?.trim().toLowerCase()
  if (!key) return undefined
  return REASONING_DISPLAY_NAMES[key] ?? titleCase(key)
}

export type SlackContextBlock = {
  type: 'context'
  elements: Array<{ type: 'mrkdwn'; text: string }>
}

export function personaFallbackNotice(
  unavailablePersonaId: string | undefined,
  personaId: string | null | undefined
): string | undefined {
  if (!unavailablePersonaId) return undefined
  return personaId
    ? `Persona "${unavailablePersonaId}" isn't available. Using "${personaId}" instead.`
    : `Persona "${unavailablePersonaId}" isn't available. Continuing without a persona.`
}

/** Builds a Slack context block containing response metadata or a notice. */
export function buildSlackResponseContextBlock(params: {
  harnessType?: string | null
  metadataEnabled?: boolean
  model?: string | null
  notice?: string | null
  reasoning?: string | null
  serviceTier?: string | null
}): SlackContextBlock | undefined {
  const includeMetadata = params.metadataEnabled === true
  const notice = params.notice?.trim()
  if (!includeMetadata && !notice) return undefined
  const segments: string[] = []
  if (notice) segments.push(`:warning: ${escapeSlackMrkdwn(notice)}`)
  if (includeMetadata) {
    const model = params.model?.trim()
    if (model) segments.push(escapeSlackMrkdwn(model.toUpperCase()))
    const harness = harnessDisplayName(params.harnessType)
    if (harness) segments.push(escapeSlackMrkdwn(harness))
    const reasoning = reasoningDisplayName(params.reasoning)
    if (reasoning) segments.push(escapeSlackMrkdwn(reasoning))
    const serviceTier = params.serviceTier?.trim()
    if (serviceTier) segments.push(escapeSlackMrkdwn(titleCase(serviceTier)))
  }
  if (segments.length === 0) return undefined
  // Middot (U+00B7) with a space on each side, matching the bot's other
  // context lines.
  return {
    type: 'context',
    elements: [{ type: 'mrkdwn', text: segments.join(' · ') }]
  }
}
