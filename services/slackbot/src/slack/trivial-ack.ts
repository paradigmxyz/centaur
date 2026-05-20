import type { NormalizedSlackEvent } from './types'

const ACK_PHRASES = new Set([
  'ok',
  'okay',
  'oki',
  'okie',
  'okies',
  'k',
  'kk',
  'kkk',
  'ty',
  'tysm',
  'thx',
  'thanks',
  'thank you',
  'thank you so much',
  'thanks so much',
  'thanks man',
  'thanks team',
  'gotcha',
  'got it',
  'noted',
  'understood',
  'sgtm',
  'lgtm',
  'np'
])

const ACK_EMOJI_ONLY_RE = /^(?:\s|👍|👌|🙏|💯|🎉|✅|❤|🤝|🫡|🔥|🙌|💪|✨|❤️)+$/u
const MAX_ACK_CHARS = 60

export function isTrivialAck(text: string): boolean {
  const trimmed = text.trim()
  if (!trimmed || trimmed.length > MAX_ACK_CHARS) return false
  if (ACK_EMOJI_ONLY_RE.test(trimmed)) return true

  const normalized = trimmed
    .toLowerCase()
    .replace(/[!?.,…]+/gu, ' ')
    .replace(/\s+/gu, ' ')
    .trim()
  return Boolean(normalized) && ACK_PHRASES.has(normalized)
}

// Mentions inside an existing thread that already has an assistant reply, with
// a short whitelisted ack as the only part, get a reaction instead of a spawn.
export function shouldAckWithReaction(event: NormalizedSlackEvent): boolean {
  if (!event.is_mention) return false
  if (event.parts.length !== 1) return false
  const part = event.parts[0]
  if (!part || part.type !== 'text') return false
  if (!isTrivialAck(part.text)) return false
  if (event.slack?.message_ts === event.thread_ts) return false
  const history = event.history_messages ?? []
  return history.some(message => message.role === 'assistant')
}
