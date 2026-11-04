/**
 * Rule-match proactivity. Once mentioned in a thread, the bot may reply to
 * later unmentioned messages there under its built-in rules. Operator rules go
 * further: the bot replies to any unmentioned message in the channel that
 * meets a rule's `when` criteria, so no mention is needed at all. Rules are
 * checked in order and the first match wins; its `context` tells the agent
 * how to respond on the turn the match starts.
 *
 * Rules are JSON files in `SLACKBOTV2_PROACTIVITY_RULES_DIR` (a mounted
 * ConfigMap), each an array of rules: `default.json` applies in every channel,
 * and `<CHANNEL_ID>.json` replaces it in that Slack conversation. An empty
 * channel list turns rule matching off there.
 */

import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { channelIdFromThreadId } from './channel-defaults'
import { isJsonObject } from './utils'

export type ProactivityRule = {
  /** Identifies the rule in logs and in what the agent is told. */
  name: string
  /** Criteria an unmentioned message must meet for the bot to reply. */
  when: string
  /** Instructions for the agent on the turn this rule's match starts. */
  context?: string
}

export type ProactivityRules = {
  default?: ProactivityRule[]
  /** Rules keyed by Slack conversation id; empty means none in that channel. */
  channels?: Record<string, ProactivityRule[]>
}

const DEFAULT_RULES_NAME = 'default'
// Each rule is a question in one classifier request, so the list bounds its
// size and latency. Keep in sync with the chart schema's maxItems.
const MAX_PROACTIVITY_RULES = 20
const RULES_FILE_SUFFIX = '.json'

/** Reads the rule files in a directory. Throws when any cannot be read or parsed. */
export function loadProactivityRules(dir: string): ProactivityRules {
  const rules: ProactivityRules = { channels: {} }
  // A mounted ConfigMap also holds dot-prefixed bookkeeping entries.
  const names = readdirSync(dir).filter(name =>
    !name.startsWith('.') && name.endsWith(RULES_FILE_SUFFIX)
  )
  for (const name of names) {
    const key = name.slice(0, -RULES_FILE_SUFFIX.length)
    if (!key) continue
    const list = parseRules(readFileSync(join(dir, name), 'utf8'), name)
    if (key === DEFAULT_RULES_NAME) {
      rules.default = list
    } else {
      rules.channels![key] = list
    }
  }
  return rules
}

/** The rules that apply to a thread, in order, or undefined when none do. */
export function resolveProactivityRules(
  rules: ProactivityRules | undefined,
  threadId: string
): ProactivityRule[] | undefined {
  const channelId = channelIdFromThreadId(threadId)
  const channel = channelId === undefined ? undefined : rules?.channels?.[channelId]
  const resolved = channel ?? rules?.default
  return resolved && resolved.length > 0 ? resolved : undefined
}

function parseRules(raw: string, file: string): ProactivityRule[] {
  const parsed: unknown = raw.trim() ? JSON.parse(raw) : []
  if (!Array.isArray(parsed)) throw new Error(`${file}: expected a JSON array of rules`)
  if (parsed.length > MAX_PROACTIVITY_RULES) {
    throw new Error(`${file}: more than ${MAX_PROACTIVITY_RULES} rules`)
  }
  const names = new Set<string>()
  return parsed.map((item, index) => {
    const name = isJsonObject(item) && typeof item.name === 'string' ? item.name.trim() : ''
    const when = isJsonObject(item) && typeof item.when === 'string' ? item.when.trim() : ''
    if (!name || !when) throw new Error(`${file}: rule ${index + 1} needs a name and when`)
    if (names.has(name)) throw new Error(`${file}: duplicate rule name ${JSON.stringify(name)}`)
    names.add(name)
    const context = isJsonObject(item) && typeof item.context === 'string'
      ? item.context.trim()
      : ''
    return context ? { context, name, when } : { name, when }
  })
}
