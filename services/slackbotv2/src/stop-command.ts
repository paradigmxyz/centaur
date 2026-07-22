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

export function isSlackStopCommand(message: { text: string }): boolean {
  const text = message.text.trim()
  if (!text) return false
  // The Chat SDK normalizes Slack mention tokens before handlers run:
  // <@U123|name> becomes @name and the bot's own <@U123> becomes @U123, so
  // message.text never contains raw <@...> tokens. Strip both raw tokens
  // (defensive) and normalized standalone @mentions; mid-word @ (emails
  // like user@example.com) is left alone.
  const withoutMentions = text
    .replace(/<@[A-Z0-9]+(?:\|[^>]+)?>/g, ' ')
    .replace(/(^|\s)@[A-Za-z0-9._-]+/g, '$1')
    .replace(/\s+/g, ' ')
    .trim()
  return STOP_COMMAND_PATTERN.test(withoutMentions)
}
