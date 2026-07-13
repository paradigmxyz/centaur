import type { Logger, Message } from 'chat'
import { withSlackApiTimeout } from './session-api'
import type { JsonValue, SlackbotV2Options } from './types'
import { isJsonObject, stringValue } from './utils'

type RawSlackBotProfile = {
  app_id?: JsonValue
  id?: JsonValue
  user_id?: JsonValue
}

type RawSlackEvent = {
  app_id?: JsonValue
  bot_id?: JsonValue
  bot_profile?: RawSlackBotProfile
  source_team?: JsonValue
  subtype?: JsonValue
  team?: JsonValue
  team_id?: JsonValue
  user?: JsonValue
  user_team?: JsonValue
}

type RawSlackEnvelope = {
  event?: JsonValue
  event_id?: JsonValue
  team_id?: JsonValue
  type?: JsonValue
}

type TriggerBotIdentity = {
  appId?: string
  userId?: string
}

const triggerBotIdentityCaches = new WeakMap<
  SlackbotV2Options,
  Map<string, Promise<TriggerBotIdentity | null>>
>()
const triggerBotUserAppCaches = new WeakMap<
  SlackbotV2Options,
  Map<string, Promise<string | null>>
>()

export function isAllowedSlackWebhookBody(
  rawBody: string,
  options: SlackbotV2Options,
  logger: Logger
): boolean {
  let payload: unknown
  try {
    payload = JSON.parse(rawBody)
  } catch {
    return true
  }
  if (!isRawSlackEnvelope(payload) || payload.type !== 'event_callback') return true
  const event = isRawSlackEvent(payload.event) ? payload.event : undefined
  if (!event) return true

  const allowedExternalTeamIds =
    options.allowedExternalTeamIds ?? splitEnvList(process.env.SLACKBOT_EXTERNAL_ORG_ALLOWLIST)
  const externalTeamId = externalSlackTeamIdForHome(stringValue(payload.team_id), event)
  if (externalTeamId && !new Set(allowedExternalTeamIds).has(externalTeamId)) {
    logger.warn('slackbotv2_event_ignored_external_org_not_allowlisted', {
      event_id: stringValue(payload.event_id),
      external_team_id: externalTeamId,
      team_id: stringValue(payload.team_id)
    })
    return false
  }
  return true
}

export async function isAllowedSlackMessage(
  message: Message,
  options: SlackbotV2Options,
  logger: Logger
): Promise<boolean> {
  const raw = isRawSlackEvent(message.raw) ? message.raw : undefined
  const allowedExternalTeamIds =
    options.allowedExternalTeamIds ?? splitEnvList(process.env.SLACKBOT_EXTERNAL_ORG_ALLOWLIST)
  const externalTeamId = raw ? externalSlackTeamId(raw) : undefined
  if (externalTeamId && !new Set(allowedExternalTeamIds).has(externalTeamId)) {
    logger.warn('slackbotv2_event_ignored_external_org_not_allowlisted', {
      external_team_id: externalTeamId,
      message_id: message.id,
      thread_id: message.threadId
    })
    return false
  }

  const triggerBotAllowlist =
    options.triggerBotAllowlist ?? splitEnvList(process.env.SLACKBOT_TRIGGER_BOT_ALLOWLIST)
  const botAuthored = message.author.isBot === true || (raw ? isBotAuthoredSlackEvent(raw) : false)
  if (
    botAuthored &&
    !(raw && (await isAllowedTriggerBotMessage(raw, triggerBotAllowlist, options, logger)))
  ) {
    logger.warn('slackbotv2_event_ignored_bot_not_allowlisted', {
      message_id: message.id,
      thread_id: message.threadId
    })
    return false
  }

  return true
}

function externalSlackTeamId(event: RawSlackEvent): string | undefined {
  return externalSlackTeamIdForHome(stringValue(event.team_id), event)
}

function externalSlackTeamIdForHome(
  homeTeamId: string | undefined,
  event: RawSlackEvent
): string | undefined {
  if (!homeTeamId) return undefined
  for (const candidate of [event.user_team, event.source_team, event.team]) {
    const teamId = stringValue(candidate)
    if (teamId && teamId !== homeTeamId) return teamId
  }
  return undefined
}

function isBotAuthoredSlackEvent(event: RawSlackEvent): boolean {
  return Boolean(event.bot_id || event.bot_profile || event.subtype === 'bot_message')
}

async function isAllowedTriggerBotMessage(
  event: RawSlackEvent,
  allowlist: readonly string[] | undefined,
  options: SlackbotV2Options,
  logger: Logger
): Promise<boolean> {
  if (!allowlist?.length) return false
  const botIds = normalizedIdentifierSet(stringValue(event.bot_id), stringValue(event.bot_profile?.id))
  const botUserIds = normalizedIdentifierSet(
    stringValue(event.user),
    stringValue(event.bot_profile?.user_id)
  )
  const allowedUserIds = new Set(allowlist.map(entry => entry.trim()).filter(isSlackMemberId))
  if (!allowedUserIds.size) return false
  if ([...botUserIds].some(userId => allowedUserIds.has(userId))) return true

  for (const botId of botIds) {
    const identity = await resolveTriggerBotIdentity(botId, options, logger)
    if (!identity) continue
    if (identity.userId && allowedUserIds.has(identity.userId)) return true
    if (!identity.appId) continue
    for (const userId of allowedUserIds) {
      if (identity.appId === await resolveTriggerBotUserAppId(userId, options, logger)) return true
    }
  }
  return false
}

function isSlackMemberId(value: string): boolean {
  return /^[UW][A-Z0-9]+$/i.test(value)
}

async function resolveTriggerBotIdentity(
  botId: string,
  options: SlackbotV2Options,
  logger: Logger
): Promise<TriggerBotIdentity | null> {
  let cache = triggerBotIdentityCaches.get(options)
  if (!cache) {
    cache = new Map()
    triggerBotIdentityCaches.set(options, cache)
  }
  const cached = cache.get(botId)
  if (cached) return cached

  const lookup = fetchTriggerBotIdentity(botId, options, logger)
  cache.set(botId, lookup)
  void lookup.then(identity => {
    if (!identity && cache.get(botId) === lookup) cache.delete(botId)
  })
  return lookup
}

async function fetchTriggerBotIdentity(
  botId: string,
  options: SlackbotV2Options,
  logger: Logger
): Promise<TriggerBotIdentity | null> {
  try {
    const url = new URL('bots.info', options.slackApiUrl ?? 'https://slack.com/api/')
    url.searchParams.set('bot', botId)
    return await withSlackApiTimeout(options, 'Slack API bots.info', async () => {
      const response = await (options.fetch ?? fetch)(url, {
        method: 'GET',
        headers: { authorization: `Bearer ${options.botToken}` }
      })
      const payload: unknown = await response.json()
      if (!response.ok || !isJsonObject(payload) || payload.ok === false || !isJsonObject(payload.bot)) {
        return null
      }
      return {
        appId: stringValue(payload.bot.app_id),
        userId: stringValue(payload.bot.user_id)
      }
    })
  } catch (error) {
    logger.warn('slackbotv2_trigger_bot_identity_lookup_failed', {
      bot_id: botId,
      error: error instanceof Error ? error.message : String(error)
    })
    return null
  }
}

async function resolveTriggerBotUserAppId(
  userId: string,
  options: SlackbotV2Options,
  logger: Logger
): Promise<string | null> {
  let cache = triggerBotUserAppCaches.get(options)
  if (!cache) {
    cache = new Map()
    triggerBotUserAppCaches.set(options, cache)
  }
  const cached = cache.get(userId)
  if (cached) return cached

  const lookup = fetchTriggerBotUserAppId(userId, options, logger)
  cache.set(userId, lookup)
  void lookup.then(appId => {
    if (!appId && cache.get(userId) === lookup) cache.delete(userId)
  })
  return lookup
}

async function fetchTriggerBotUserAppId(
  userId: string,
  options: SlackbotV2Options,
  logger: Logger
): Promise<string | null> {
  try {
    const url = new URL('users.info', options.slackApiUrl ?? 'https://slack.com/api/')
    url.searchParams.set('user', userId)
    return await withSlackApiTimeout(options, 'Slack API users.info', async () => {
      const response = await (options.fetch ?? fetch)(url, {
        method: 'GET',
        headers: { authorization: `Bearer ${options.botToken}` }
      })
      const payload: unknown = await response.json()
      if (
        !response.ok ||
        !isJsonObject(payload) ||
        payload.ok === false ||
        !isJsonObject(payload.user) ||
        !isJsonObject(payload.user.profile)
      ) {
        return null
      }
      return stringValue(payload.user.profile.api_app_id) ?? null
    })
  } catch (error) {
    logger.warn('slackbotv2_trigger_bot_user_identity_lookup_failed', {
      user_id: userId,
      error: error instanceof Error ? error.message : String(error)
    })
    return null
  }
}

function normalizedIdentifierSet(...values: Array<string | undefined>): Set<string> {
  return new Set(values.map(value => value?.trim()).filter((value): value is string => Boolean(value)))
}

function splitEnvList(value: string | undefined): string[] {
  return (value ?? '')
    .split(/[\s,]+/)
    .map(part => part.trim())
    .filter(Boolean)
}

function isRawSlackEvent(value: unknown): value is RawSlackEvent {
  return isJsonObject(value) && (value.bot_profile === undefined || isJsonObject(value.bot_profile))
}

function isRawSlackEnvelope(value: unknown): value is RawSlackEnvelope {
  return isJsonObject(value)
}
