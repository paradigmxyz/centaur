/**
 * Prompt-match proactivity. Once mentioned in a thread, the bot may reply to
 * later unmentioned messages there under its built-in rules. A prompt goes
 * further: the bot replies to any unmentioned message in the channel that
 * meets the prompt's criteria, so no mention is needed at all.
 *
 * Prompts are files in `SLACKBOTV2_PROACTIVITY_PROMPTS_DIR` (a mounted
 * ConfigMap): `default.md` applies in every channel, and `<CHANNEL_ID>.md`
 * replaces it in that Slack conversation. An empty channel file turns prompt
 * matching off there.
 */

import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { channelIdFromThreadId } from './channel-defaults'

export type ProactivityPrompts = {
  default?: string
  /** Prompts keyed by Slack conversation id; empty means none in that channel. */
  channels?: Record<string, string>
}

const DEFAULT_PROMPT_NAME = 'default'
const PROMPT_FILE_SUFFIX = '.md'

/** Reads the prompt files in a directory. Throws when it cannot be read. */
export function loadProactivityPrompts(dir: string): ProactivityPrompts {
  const prompts: ProactivityPrompts = { channels: {} }
  // A mounted ConfigMap also holds dot-prefixed bookkeeping entries.
  const names = readdirSync(dir).filter(name =>
    !name.startsWith('.') && name.endsWith(PROMPT_FILE_SUFFIX)
  )
  for (const name of names) {
    const key = name.slice(0, -PROMPT_FILE_SUFFIX.length)
    const prompt = readFileSync(join(dir, name), 'utf8').trim()
    if (key === DEFAULT_PROMPT_NAME) {
      prompts.default = prompt || undefined
    } else if (key) {
      prompts.channels![key] = prompt
    }
  }
  return prompts
}

/** The prompt that applies to a thread, or undefined when none does. */
export function resolveProactivityPrompt(
  prompts: ProactivityPrompts | undefined,
  threadId: string
): string | undefined {
  const channelId = channelIdFromThreadId(threadId)
  const channel = channelId === undefined ? undefined : prompts?.channels?.[channelId]
  return (channel ?? prompts?.default)?.trim() || undefined
}
