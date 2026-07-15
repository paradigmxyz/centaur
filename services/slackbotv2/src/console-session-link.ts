/**
 * Slack-only "Open chat in Console" context line.
 *
 * On the first assistant message in a Slack thread, slackbotv2 appends a Block
 * Kit `context` block linking to the Console session view. The block is passed
 * to the chat adapter via `StreamOptions.stopBlocks`, which the adapter forwards
 * to Slack's `chat.stopStream` `blocks` argument ("Block formatted elements will
 * be appended to the end of the message"). This keeps the rendering Slack-only
 * and out of the shared `@centaur/rendering` package used by Discord/Teams.
 */

import claudeSettings from '../../../harness/claude/settings.json'
import codexConfig from '../../../harness/codex/config.toml'

const HARNESS_DISPLAY_NAMES: Record<string, string> = {
  amp: 'Amp',
  claudecode: 'Claude Code',
  codex: 'Codex'
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
  codex:
    typeof (codexConfig as { model?: unknown }).model === 'string'
      ? ((codexConfig as { model: string }).model)
      : undefined
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

/**
 * Builds the Console session URL for a Slack thread key, or undefined when no
 * Console base URL is configured (in which case no link/block should render).
 * The thread key is the exact value slackbotv2 sends as `thread_key` to the
 * session API, URL-encoded into the `thread` query parameter the Console reads.
 */
export function consoleSessionUrl(
  consoleBaseUrl: string | null | undefined,
  threadKey: string
): string | undefined {
  const base = consoleBaseUrl?.trim()
  if (!base) return undefined
  const normalized = base.replace(/\/+$/, '')
  return `${normalized}/console/threads?thread=${encodeURIComponent(threadKey)}`
}

export type SlackContextBlock = {
  type: 'context'
  elements: Array<{ type: 'mrkdwn'; text: string }>
}

/**
 * Builds the "Open chat in Console · {MODEL} · {Harness}" context block, or
 * undefined when no Console base URL is configured (a bare "Open chat in
 * Console" with no link is pointless, so the whole block is skipped). The
 * model id is uppercased for display.
 */
export function buildConsoleSessionContextBlock(params: {
  consoleBaseUrl: string | null | undefined
  threadKey: string
  harnessType?: string | null
  model?: string | null
}): SlackContextBlock | undefined {
  const url = consoleSessionUrl(params.consoleBaseUrl, params.threadKey)
  if (!url) return undefined
  const segments = [`<${url}|Open chat in Console>`]
  const model = params.model?.trim()
  if (model) segments.push(escapeSlackMrkdwn(model.toUpperCase()))
  const harness = harnessDisplayName(params.harnessType)
  if (harness) segments.push(escapeSlackMrkdwn(harness))
  // Middot (U+00B7) with a space on each side, matching the bot's other
  // context lines.
  return {
    type: 'context',
    elements: [{ type: 'mrkdwn', text: segments.join(' · ') }]
  }
}
