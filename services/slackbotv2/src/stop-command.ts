import { escapeRegExp } from './utils'

const STOP_COMMAND_PATTERN = new RegExp(
  [
    String.raw`^`,
    String.raw`(?:(?:please|pls)\s+)?`,
    String.raw`(?:(?:can|could|would|will)\s+you\s+)?`,
    String.raw`(?:stop+|kill(?:ed|ing|s)?|end(?:ed|ing|s)?|cancell?(?:ed|ing|s)?)`,
    String.raw`(?:\s+(?:it|this|that|now|please|pls|the\s+(?:run|execution|request|job|thread|turn)))*`,
    String.raw`[.!?]*$`
  ].join(''),
  'i'
)

/**
 * `botNames` are the names the bot's own mention can take in message.text:
 * its user id and its Slack display name.
 */
export function isSlackStopCommand(
  message: { text: string },
  botNames: readonly string[] = []
): boolean {
  const text = message.text.trim()
  if (!text) return false
  // The Chat SDK normalizes Slack mention tokens before handlers run:
  // <@U123> becomes @<display name>, the bot's own mention included, and an
  // id it cannot look up stays @U123, so message.text never contains raw
  // <@...> tokens. A display name can have several words (`@mybot (dev)`),
  // so the bot's own names are stripped whole first. Then strip raw tokens
  // (defensive) and other standalone @mentions; mid-word @ (emails like
  // user@example.com) is left alone.
  let withoutMentions = text.replace(/<@[A-Z0-9]+(?:\|[^>]+)?>/g, ' ')
  for (const name of botNames) {
    withoutMentions = withoutMentions.replace(
      new RegExp(`(^|\\s)@${escapeRegExp(name)}(?=\\s|$)`, 'gi'),
      '$1'
    )
  }
  withoutMentions = withoutMentions
    .replace(/(^|\s)@[A-Za-z0-9._-]+/g, '$1')
    .replace(/\s+/g, ' ')
    .trim()
  return STOP_COMMAND_PATTERN.test(withoutMentions)
}
