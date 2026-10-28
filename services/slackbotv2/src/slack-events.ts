import type { Logger, Message } from 'chat'
import { fetchSlackExternalTeamIds, withSlackApiTimeout } from './session-api'
import type { JsonValue, SlackbotV2Options } from './types'
import { errorMessage, isJsonObject, nowMs, stringValue } from './utils'

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

type RawSlackInteraction = {
  actions?: JsonValue
  team?: JsonValue
  type?: JsonValue
  user?: JsonValue
}

type TriggerBotIdentity = {
  appId?: string
  userId?: string
}

type ExternalTeamIdsCache = {
  expiresAtMs: number
  lookup?: Promise<ReadonlySet<string>>
  /** Last list api-rs returned; kept when a refresh fails. */
  teamIds?: ReadonlySet<string>
}

/**
 * The Slack Connect allowlist has never loaded, so an external org's event
 * can be neither admitted nor safely dropped. The webhook answers 503: Slack
 * redelivers Events API events, but not interactions such as button clicks,
 * which the user has to repeat.
 */
export class SlackExternalTeamIdsUnavailableError extends Error {
  constructor(cause: unknown) {
    super(`Slack Connect allowlist is unavailable: ${errorMessage(cause)}`)
    this.name = 'SlackExternalTeamIdsUnavailableError'
  }
}

const EXTERNAL_TEAM_IDS_TTL_MS = 30_000
const EXTERNAL_TEAM_IDS_RETRY_MS = 5_000

const externalTeamIdsCaches = new WeakMap<SlackbotV2Options, ExternalTeamIdsCache>()
const triggerBotIdentityCaches = new WeakMap<
  SlackbotV2Options,
  Map<string, Promise<TriggerBotIdentity | null>>
>()
const triggerBotUserAppCaches = new WeakMap<
  SlackbotV2Options,
  Map<string, Promise<string | null>>
>()

export async function isAllowedSlackWebhookBody(
  rawBody: string,
  options: SlackbotV2Options,
  logger: Logger
): Promise<boolean> {
  const payload = parseSlackWebhookPayload(rawBody)
  if (!payload) return true
  if (isRawSlackInteraction(payload) && payload.type === 'block_actions') {
    return isAllowedSlackInteraction(payload, options, logger)
  }
  if (!isRawSlackEnvelope(payload) || payload.type !== 'event_callback') return true
  const event = isRawSlackEvent(payload.event) ? payload.event : undefined
  if (!event) return true

  const externalTeamId = externalSlackTeamIdForHome(stringValue(payload.team_id), event)
  if (externalTeamId && !(await allowedExternalTeamIds(options, logger)).has(externalTeamId)) {
    logger.warn('slackbotv2_event_ignored_external_org_not_allowlisted', {
      event_id: stringValue(payload.event_id),
      external_team_id: externalTeamId,
      team_id: stringValue(payload.team_id)
    })
    return false
  }
  return true
}

export function parseSlackWebhookPayload(rawBody: string): Record<string, unknown> | null {
  const parsed = parseJsonObject(rawBody)
  if (parsed) return parsed
  const formPayload = new URLSearchParams(rawBody).get('payload')
  return formPayload ? parseJsonObject(formPayload) : null
}

function parseJsonObject(value: string): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(value)
    return isJsonObject(parsed) ? parsed : null
  } catch {
    return null
  }
}

async function isAllowedSlackInteraction(
  payload: RawSlackInteraction,
  options: SlackbotV2Options,
  logger: Logger
): Promise<boolean> {
  const team = isJsonObject(payload.team) ? payload.team : undefined
  const user = isJsonObject(payload.user) ? payload.user : undefined
  const homeTeamId = stringValue(team?.id)
  const externalTeamId = externalSlackTeamIdForHome(homeTeamId, {
    user_team: user?.team_id
  })
  if (!externalTeamId || (await allowedExternalTeamIds(options, logger)).has(externalTeamId)) {
    return true
  }

  const actions = Array.isArray(payload.actions) ? payload.actions : []
  const firstAction = actions.find(isJsonObject)
  logger.warn('slackbotv2_event_ignored_external_org_not_allowlisted', {
    action_id: firstAction ? stringValue(firstAction.action_id) : undefined,
    external_team_id: externalTeamId,
    team_id: homeTeamId
  })
  return false
}

export async function isAllowedSlackMessage(
  message: Message,
  options: SlackbotV2Options,
  logger: Logger
): Promise<boolean> {
  const raw = isRawSlackEvent(message.raw) ? message.raw : undefined
  const externalTeamId = raw ? externalSlackTeamId(raw) : undefined
  if (externalTeamId && !(await isAllowedExternalTeamForMessage(externalTeamId, options, logger))) {
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

/**
 * The webhook gate already answered 503 if the list was unavailable, so a
 * failure here only denies.
 */
async function isAllowedExternalTeamForMessage(
  externalTeamId: string,
  options: SlackbotV2Options,
  logger: Logger
): Promise<boolean> {
  try {
    return (await allowedExternalTeamIds(options, logger)).has(externalTeamId)
  } catch (error) {
    if (!(error instanceof SlackExternalTeamIdsUnavailableError)) throw error
    return false
  }
}

/**
 * Slack's `chat.startStream` only accepts a `U...`/`W...` member id as
 * `recipient_user_id`. A bot-authored message without `user` or
 * `bot_profile.user_id` carries the bot's `B...` id instead, so resolve the
 * bot's member id from the `bots.info` lookup the allowlist gate caches.
 */
export async function slackStreamRecipientUserId(
  author: { isBot: boolean | 'unknown'; userId: string },
  options: SlackbotV2Options,
  logger: Logger
): Promise<string> {
  if (author.isBot !== true || isSlackMemberId(author.userId)) return author.userId
  const identity = await resolveTriggerBotIdentity(author.userId, options, logger)
  return identity?.userId && isSlackMemberId(identity.userId) ? identity.userId : author.userId
}

/**
 * Console-managed Slack Connect allowlist, cached briefly. A failed refresh
 * keeps the last known list and retries sooner; with no list yet it throws
 * {@link SlackExternalTeamIdsUnavailableError}.
 */
async function allowedExternalTeamIds(
  options: SlackbotV2Options,
  logger: Logger
): Promise<ReadonlySet<string>> {
  if (options.allowedExternalTeamIds) return new Set(options.allowedExternalTeamIds)
  const cache = externalTeamIdsCaches.get(options) ?? { expiresAtMs: 0 }
  externalTeamIdsCaches.set(options, cache)
  if (cache.lookup && nowMs() < cache.expiresAtMs) return cache.lookup

  cache.expiresAtMs = nowMs() + EXTERNAL_TEAM_IDS_TTL_MS
  const lookup = fetchSlackExternalTeamIds(options).then(
    teamIds => (cache.teamIds = new Set(teamIds)),
    error => {
      logger.warn('slackbotv2_external_team_ids_lookup_failed', { error: errorMessage(error) })
      cache.expiresAtMs = nowMs() + EXTERNAL_TEAM_IDS_RETRY_MS
      if (cache.teamIds) return cache.teamIds
      throw new SlackExternalTeamIdsUnavailableError(error)
    }
  )
  cache.lookup = lookup
  return lookup
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

function isRawSlackInteraction(value: unknown): value is RawSlackInteraction {
  return isJsonObject(value)
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
  const allowedBotIds = new Set(
    allowlist
      .map(entry => entry.trim())
      .filter(entry => entry.startsWith('bot:'))
      .map(entry => entry.slice('bot:'.length))
      .filter(isSlackBotId)
  )
  if ([...botIds].some(botId => allowedBotIds.has(botId))) return true

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

function isSlackBotId(value: string): boolean {
  return /^B[A-Z0-9]+$/i.test(value)
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

export function splitEnvList(value: string | undefined): string[] {
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
