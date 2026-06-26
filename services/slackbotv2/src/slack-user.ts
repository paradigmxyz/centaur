import { isJsonObject, stringValue } from './utils'

type MessageWithSlackUser = {
  author?: {
    userId?: unknown
  }
  raw?: unknown
}

export function slackUserIdForMessage(message: MessageWithSlackUser): string | undefined {
  return stringValue(message.author?.userId) ?? rawSlackUserId(message.raw)
}

export function rawSlackUserId(raw: unknown): string | undefined {
  if (!isJsonObject(raw)) return undefined
  const directUser = stringValue(raw.user)
  if (directUser) return directUser
  const user = raw.user
  if (isJsonObject(user)) {
    return stringValue(user.id) ?? stringValue(user.user_id)
  }
  const botProfile = raw.bot_profile
  if (isJsonObject(botProfile)) return stringValue(botProfile.user_id)
  return undefined
}
