import { isJsonObject, stringValue } from './utils'

type ResolveSlackBotUserIdOptions = {
  botToken: string
  configuredBotUserId?: string
  fetchFn?: typeof fetch
  slackApiUrl?: string
  timeoutMs?: number
}

const DEFAULT_SLACK_AUTH_TIMEOUT_MS = 5_000

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

export async function resolveSlackBotUserId(
  options: ResolveSlackBotUserIdOptions
): Promise<string> {
  const configuredBotUserId = options.configuredBotUserId?.trim()
  if (configuredBotUserId) return configuredBotUserId

  const url = new URL('auth.test', options.slackApiUrl ?? 'https://slack.com/api/')
  const response = await (options.fetchFn ?? fetch)(url, {
    method: 'POST',
    headers: { authorization: `Bearer ${options.botToken}` },
    signal: AbortSignal.timeout(options.timeoutMs ?? DEFAULT_SLACK_AUTH_TIMEOUT_MS)
  })
  const payload = await response.json()
  const userId = isJsonObject(payload) ? stringValue(payload.user_id) : undefined
  if (!response.ok || !isJsonObject(payload) || payload.ok !== true || !userId) {
    const reason = isJsonObject(payload) ? stringValue(payload.error) : undefined
    throw new Error(`Slack auth.test failed${reason ? `: ${reason}` : ''}`)
  }
  return userId
}
