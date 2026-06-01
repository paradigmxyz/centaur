import type { Logger, Message } from 'chat'
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

export function isAllowedSlackMessage(
  message: Message,
  options: SlackbotV2Options,
  logger: Logger
): boolean {
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
  if (botAuthored && !(raw && isAllowedTriggerBotMessage(raw, triggerBotAllowlist))) {
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

function isAllowedTriggerBotMessage(
  event: RawSlackEvent,
  allowlist: readonly string[] | undefined
): boolean {
  if (!allowlist?.length) return false
  const appIds = normalizedIdentifierSet(stringValue(event.app_id), stringValue(event.bot_profile?.app_id))
  const botIds = normalizedIdentifierSet(stringValue(event.bot_id), stringValue(event.bot_profile?.id))
  const botUserIds = normalizedIdentifierSet(
    stringValue(event.user),
    stringValue(event.bot_profile?.user_id)
  )
  const anyIds = new Set([...appIds, ...botIds, ...botUserIds])

  for (const entry of allowlist) {
    const parsed = parseTriggerBotAllowlistEntry(entry)
    if (!parsed) continue
    if (parsed.kind === 'app' && appIds.has(parsed.value)) return true
    if (parsed.kind === 'bot' && botIds.has(parsed.value)) return true
    if (parsed.kind === 'user' && botUserIds.has(parsed.value)) return true
    if (parsed.kind === 'any' && anyIds.has(parsed.value)) return true
  }
  return false
}

function normalizedIdentifierSet(...values: Array<string | undefined>): Set<string> {
  return new Set(values.map(value => value?.trim()).filter((value): value is string => Boolean(value)))
}

function parseTriggerBotAllowlistEntry(
  entry: string
): { kind: 'app' | 'bot' | 'user' | 'any'; value: string } | null {
  const trimmed = entry.trim()
  if (!trimmed) return null
  const prefixed = /^(app|bot|user):(.+)$/i.exec(trimmed)
  if (!prefixed) return { kind: 'any', value: trimmed }
  const kind = prefixed[1]
  const value = prefixed[2]?.trim()
  if (!kind || !value) return null
  return { kind: kind.toLowerCase() as 'app' | 'bot' | 'user', value }
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
