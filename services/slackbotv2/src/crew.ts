import type { SlackbotV2Options } from './types'

export type CrewMember = {
  id: string
  appId: string
  teamId: string
  botUserId: string
  botToken: string
  signingSecret: string
}

/** Operator-managed configuration, never user message input. Errors omit secrets. */
export function parseCrew(raw: string): CrewMember[] {
  let values: unknown
  try { values = JSON.parse(raw) } catch { throw new Error('Crew file must contain a JSON array') }
  if (!Array.isArray(values)) throw new Error('Crew file must contain a JSON array')
  const ids = new Set<string>()
  const apps = new Set<string>()
  return values.map(value => {
    if (!value || typeof value !== 'object') throw new Error('Invalid Crew entry')
    const checks: Record<string, RegExp> = {
      id: /^[a-z][a-z0-9-]{0,47}$/, appId: /^A[A-Z0-9]+$/, teamId: /^T[A-Z0-9]+$/,
      botUserId: /^[UW][A-Z0-9]+$/, botToken: /^xoxb-\S+$/, signingSecret: /^\S+$/
    }
    for (const [key, pattern] of Object.entries(checks)) {
      if (typeof value[key] !== 'string' || !pattern.test(value[key])) throw new Error(`Invalid Crew ${key}`)
    }
    if (ids.has(value.id) || apps.has(value.appId)) throw new Error('Duplicate Crew id or appId')
    ids.add(value.id)
    apps.add(value.appId)
    return Object.fromEntries(Object.keys(checks).map(key => [key, value[key]])) as CrewMember
  })
}

export function crewOptions(base: SlackbotV2Options, member: CrewMember): SlackbotV2Options {
  const { state: _state, ...options } = base
  return {
    ...options, botToken: member.botToken, botUserId: member.botUserId,
    signingSecret: member.signingSecret, slackHomeTeamId: member.teamId,
    userName: member.id, crew: { id: member.id, appId: member.appId },
    agentViewEnabled: false,
    activitySummaryStatusEnabled: false,
    autoJoinCreatedChannels: false,
    stateKeyPrefix: `${base.stateKeyPrefix ?? 'centaur-slackbotv2'}:crew:${member.appId}`
  }
}

/** Scope the durable session while preserving its Slack destination. */
export function crewSessionKey(options: SlackbotV2Options, threadId: string): string {
  if (!options.crew) return threadId
  const parts = threadId.split(':')
  const tail = parts.slice(-2)
  if (parts[0] !== 'slack' || ![3, 4].includes(parts.length)
      || !/^[CDG][A-Z0-9]+$/.test(tail[0]!) || !/^\d+\.\d+$/.test(tail[1]!)) {
    throw new Error('Invalid Slack thread key for Crew')
  }
  const team = parts.length === 4 ? parts[1] : options.slackHomeTeamId
  if (!team || team !== options.slackHomeTeamId) throw new Error('Crew workspace mismatch')
  return `slack:${team}:${options.crew.appId}:${tail.join(':')}`
}

type FetchApp = { fetch: (request: Request) => Response | Promise<Response> }

/** Each route delegates signature verification to its own Slack adapter. */
export function crewFetch(defaultApp: FetchApp, members: Map<string, FetchApp>) {
  return (request: Request): Response | Promise<Response> => {
    const url = new URL(request.url)
    const prefix = '/api/webhooks/slack/crew/'
    if (!url.pathname.startsWith(prefix)) return defaultApp.fetch(request)
    const app = members.get(url.pathname.slice(prefix.length))
    if (!app) return new Response('Unknown Crew member', { status: 404 })
    url.pathname = '/api/webhooks/slack'
    return app.fetch(new Request(url, request))
  }
}
