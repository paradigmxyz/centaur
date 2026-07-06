const STOP_COMMAND_PATTERN = new RegExp(
  [
    String.raw`(?:^|[^A-Za-z0-9_-])`,
    String.raw`(?:stop+|kill(?:ed|ing|s)?|end(?:ed|ing|s)?|cancell?(?:ed|ing|s)?)`,
    String.raw`(?=$|[^A-Za-z0-9_-])`
  ].join(''),
  'i'
)

export function isSlackStopCommand(message: { text: string }): boolean {
  const text = message.text.trim()
  if (!text) return false
  const withoutMentions = text.replace(/<@[A-Z0-9]+(?:\|[^>]+)?>/g, ' ').trim()
  return STOP_COMMAND_PATTERN.test(withoutMentions)
}
