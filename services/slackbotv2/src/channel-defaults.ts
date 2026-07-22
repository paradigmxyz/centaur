/**
 * Per-channel default harness / model / provider / reasoning. Loaded from the
 * `SLACKBOTV2_CHANNEL_DEFAULTS` env var: JSON keyed by Slack conversation id,
 * each value an object normalized like the inline flags (see
 * `normalizeHarnessOverrides`):
 *
 *   SLACKBOTV2_CHANNEL_DEFAULTS='{
 *     "C0ENG":     {"harness": "claude", "model": "opus", "reasoning": "high"},
 *     "C0TRIAGE":  {"reasoning": "low"},
 *     "C0BEDROCK": {"provider": "bedrock", "model": "gpt-5.2"}
 *   }'
 *
 * Fields are independent. Precedence (in index.ts): per-thread override, then
 * channel default, then deployment default. Setting `harness` restarts a thread
 * onto it like `--claude`/`--codex`; `reasoning` only affects codex.
 */

import { normalizeHarnessOverrides, type HarnessOverrides } from './overrides'

export type ChannelDefaults = Record<string, HarnessOverrides>

/**
 * Parses `SLACKBOTV2_CHANNEL_DEFAULTS` into a channel→overrides map (empty for
 * unset input). Never throws — bad JSON or entries are skipped and reported via
 * `onError`.
 */
export function parseChannelDefaults(
  raw: string | undefined,
  onError?: (message: string) => void
): ChannelDefaults {
  const trimmed = raw?.trim()
  if (!trimmed) return {}
  let parsed: unknown
  try {
    parsed = JSON.parse(trimmed)
  } catch (error) {
    onError?.(`invalid JSON: ${error instanceof Error ? error.message : String(error)}`)
    return {}
  }
  if (!isPlainObject(parsed)) {
    onError?.('expected a JSON object keyed by channel id')
    return {}
  }
  const result: ChannelDefaults = {}
  for (const [channelId, rawEntry] of Object.entries(parsed)) {
    const key = channelId.trim()
    if (!key) continue
    if (!isPlainObject(rawEntry)) {
      onError?.(`channel ${key}: expected an object of harness/model/provider/reasoning fields`)
      continue
    }
    const overrides = normalizeHarnessOverrides(rawEntry, message => onError?.(`channel ${key}: ${message}`))
    if (!overrides.harnessType && !overrides.model && !overrides.provider && !overrides.reasoning) {
      onError?.(`channel ${key}: no usable harness/model/provider/reasoning fields`)
      continue
    }
    result[key] = overrides
  }
  return result
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * Extracts the Slack conversation id from a thread key of the shape
 * `slack:CHANNEL[:THREAD_TS]` (or `slack:TEAM:CHANNEL:…`), mirroring the
 * classification in session-api's `slackConversationId`: the first segment
 * after the namespace whose first character is `C`, `G`, or `D`.
 */
export function channelIdFromThreadId(threadId: string): string | undefined {
  const segments = threadId.split(':').slice(1)
  for (const segment of segments) {
    const first = segment.charAt(0)
    if (first === 'C' || first === 'G' || first === 'D') return segment
  }
  return undefined
}

/** Resolves the channel default for a thread, or undefined when none applies. */
export function resolveChannelDefault(
  defaults: ChannelDefaults | undefined,
  threadId: string
): HarnessOverrides | undefined {
  if (!defaults) return undefined
  const channelId = channelIdFromThreadId(threadId)
  if (!channelId) return undefined
  return defaults[channelId]
}
