export function isSlackStopCommand(message: { text: string }): boolean {
  const text = message.text.trim()
  if (!text) return false
  const withoutMentions = text.replace(/<@[A-Z0-9]+(?:\|[^>]+)?>/g, ' ').trim()
  return /\bstop\b/i.test(withoutMentions)
}
