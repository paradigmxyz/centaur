import { createHash, createHmac, randomBytes, timingSafeEqual } from 'node:crypto'
import { Hono } from 'hono'
import { getCookie, setCookie, deleteCookie } from 'hono/cookie'
import { createSlackbotV2 } from './index'
import type { SlackbotV2, SlackbotV2Options } from './types'
import type { CrewRecord, CrewStore } from './crew-store'

// Match the existing ingress's context, file, streaming, and requester lookups.
// No user token, workspace administration, or public-channel auto-join scopes.
export const CREW_BOT_SCOPES = [
  'app_mentions:read', 'assistant:write', 'chat:write',
  'channels:history', 'channels:read', 'groups:history', 'groups:read',
  'im:history', 'im:read', 'im:write', 'mpim:history', 'mpim:read',
  'users:read', 'users:read.email', 'files:read'
] as const

export type CrewConfig = {
  publicUrl: string
  adminToken: string
  configurationToken: string
  teamId: string
  allowedPersonas: readonly string[]
  store: CrewStore
  botOptions: SlackbotV2Options
  fetch?: typeof globalThis.fetch
  createBot?: typeof createSlackbotV2
}

export function crewManifest(name: string, base: string, id: string) {
  const events = `${base}/api/slack/crew/${id}/events`
  return {
    display_information: { name, description: 'A member of your Centaur crew' },
    features: {
      bot_user: { display_name: name, always_online: true },
      app_home: { home_tab_enabled: false, messages_tab_enabled: true, messages_tab_read_only_enabled: false }
    },
    oauth_config: {
      redirect_urls: [`${base}/api/slack/crew/${id}/oauth`],
      scopes: { bot: CREW_BOT_SCOPES }
    },
    settings: {
      event_subscriptions: {
        request_url: events,
        bot_events: ['app_mention', 'message.channels', 'message.groups', 'message.im', 'message.mpim']
      },
      interactivity: { is_enabled: true, request_url: events },
      socket_mode_enabled: false,
      token_rotation_enabled: false
    }
  }
}

function digest(value: string): string {
  return createHash('sha256').update(value).digest('hex')
}

function matches(a: string, b: string): boolean {
  return timingSafeEqual(Buffer.from(digest(a)), Buffer.from(digest(b)))
}

function publicRecord(record: CrewRecord, base: string) {
  return {
    id: record.id, name: record.name, crew_id: record.personaId, status: record.status,
    app_id: record.appId, bot_user_id: record.botUserId, team_id: record.teamId,
    ...(record.status === 'needs_install' && record.installTicket
      ? { install_url: `${base}/api/slack/crew/${record.id}/install?ticket=${record.installTicket}` } : {})
  }
}

export function createCrewManager(config: CrewConfig) {
  const base = config.publicUrl.replace(/\/$/, '')
  const publicUrl = new URL(base)
  if (publicUrl.protocol !== 'https:' || publicUrl.username || publicUrl.password
      || publicUrl.search || publicUrl.hash || publicUrl.pathname !== '/') {
    throw new Error('SLACK_CREW_PUBLIC_URL must be an HTTPS origin')
  }
  if (!config.adminToken || !config.configurationToken || !/^T[A-Z0-9]+$/.test(config.teamId)
      || config.allowedPersonas.length === 0) throw new Error('Incomplete Crew configuration')
  const app = new Hono()
  const bots = new Map<string, Promise<SlackbotV2>>()
  const fetchFn = config.fetch ?? globalThis.fetch

  async function slack(method: string, body: Record<string, unknown>, token?: string) {
    // Never retry app creation or OAuth exchange: ambiguous success requires reconciliation.
    const response = await fetchFn(`https://slack.com/api/${method}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/x-www-form-urlencoded',
        ...(token ? { Authorization: `Bearer ${token}` } : {})
      },
      body: new URLSearchParams(Object.entries(body).map(([key, value]) => [key, String(value)])),
      signal: AbortSignal.timeout(20_000),
      redirect: 'error'
    })
    if (!response.ok) throw new Error(`Slack ${method} failed (HTTP ${response.status})`)
    const result = await response.json() as Record<string, any>
    if (result.ok !== true) {
      const code = typeof result.error === 'string' && /^[a-z_]+$/.test(result.error) ? result.error : 'unknown_error'
      throw new Error(`Slack ${method}: ${code}`)
    }
    return result
  }

  function bot(record: CrewRecord): Promise<SlackbotV2> {
    let pending = bots.get(record.id)
    if (!pending) {
      pending = Promise.resolve().then(() => {
        if (!record.botToken || !record.botUserId || !record.signingSecret || !record.appId
            || record.teamId !== config.teamId || !config.allowedPersonas.includes(record.personaId)) {
          throw new Error('Crew installation is not active or allowed')
        }
        return (config.createBot ?? createSlackbotV2)({
          ...config.botOptions,
          botToken: record.botToken, botUserId: record.botUserId,
          botAppId: record.appId, signingSecret: record.signingSecret,
          fixedPersonaId: record.personaId, slackHomeTeamId: record.teamId,
          userName: record.name,
          state: undefined,
          stateKeyPrefix: `${config.botOptions.stateKeyPrefix ?? 'centaur-slackbotv2'}:crew:${record.appId}`,
          agentViewEnabled: false, autoJoinCreatedChannels: false, steeringReactionEnabled: false,
          triggerBotAllowlist: []
        })
      })
      bots.set(record.id, pending)
      pending.catch(() => bots.delete(record.id))
    }
    return pending
  }

  app.onError((error, c) => {
    // Exceptions may include transport details. Keep responses and logs credential-free.
    config.botOptions.logger?.warn('slackbotv2_crew_request_failed', { path: c.req.path })
    return c.json({ error: error.message.startsWith('Slack ') ? error.message : 'Crew operation failed; inspect installation status before retrying.' }, 502)
  })
  app.use('/api/slack/crew', async (c, next) => {
    if (!matches(c.req.header('Authorization') ?? '', `Bearer ${config.adminToken}`)) {
      return c.json({ error: 'Unauthorized' }, 401)
    }
    return next()
  })
  app.get('/api/slack/crew', async c => c.json({ crew: (await config.store.list()).map(r => publicRecord(r, base)) }))
  app.post('/api/slack/crew', async c => {
    const input = await c.req.json().catch(() => null)
    if (!input || typeof input.id !== 'string' || !/^[a-z][a-z0-9-]{1,47}$/.test(input.id)
        || typeof input.name !== 'string' || input.name.trim().length < 1 || input.name.length > 35
        || !config.allowedPersonas.includes(input.crew_id)) {
      return c.json({ error: 'Provide id (2–48 lowercase slug characters), name (1–35 characters), and an allowed crew_id.' }, 400)
    }
    const record: CrewRecord = { id: input.id, name: input.name.trim(), personaId: input.crew_id, status: 'creating' }
    if (!await config.store.reserve(record)) {
      const existing = await config.store.get(record.id)
      if (!existing || existing.name !== record.name || existing.personaId !== record.personaId) {
        return c.json({ error: 'That id belongs to a different Crew definition.' }, 409)
      }
      // A lost create response cannot safely be retried at Slack. The durable reservation
      // remains visible for an operator to reconcile rather than spawning duplicates.
      return c.json(publicRecord(existing, base), existing.status === 'creating' ? 409 : 200)
    }
    const created = await slack('apps.manifest.create', {
      manifest: JSON.stringify(crewManifest(record.name, base, record.id))
    }, config.configurationToken)
    if (!/^A[A-Z0-9]+$/.test(created.app_id ?? '') || !created.credentials?.client_id
        || !created.credentials?.client_secret || !created.credentials?.signing_secret) {
      throw new Error('Incomplete Slack app creation response')
    }
    Object.assign(record, {
      appId: created.app_id, clientId: created.credentials.client_id,
      clientSecret: created.credentials.client_secret, signingSecret: created.credentials.signing_secret,
      installTicket: randomBytes(32).toString('hex'), status: 'needs_install'
    })
    await config.store.save(record)
    config.botOptions.logger?.info('slackbotv2_crew_app_created', {
      crew_id: record.id, profile_id: record.personaId, app_id: record.appId
    })
    return c.json(publicRecord(record, base), 201)
  })

  app.get('/api/slack/crew/:id/install', async c => {
    const state = randomBytes(32).toString('hex')
    const record = await config.store.beginInstall(c.req.param('id'), c.req.query('ticket') ?? '', digest(state))
    if (!record) return c.text('Invalid installation link', 400)
    setCookie(c, `crew_${record.id}`, state, {
      httpOnly: true, secure: true, sameSite: 'Lax', path: `/api/slack/crew/${record.id}`, maxAge: 600
    })
    c.header('Cache-Control', 'no-store')
    c.header('Referrer-Policy', 'no-referrer')
    const url = new URL('https://slack.com/oauth/v2/authorize')
    url.search = new URLSearchParams({
      client_id: record.clientId!, scope: CREW_BOT_SCOPES.join(','), state,
      team: config.teamId, redirect_uri: `${base}/api/slack/crew/${record.id}/oauth`
    }).toString()
    return c.redirect(url.toString())
  })

  app.get('/api/slack/crew/:id/oauth', async c => {
    const id = c.req.param('id')
    const state = c.req.query('state') ?? ''
    const cookie = getCookie(c, `crew_${id}`) ?? ''
    c.header('Cache-Control', 'no-store')
    c.header('Referrer-Policy', 'no-referrer')
    if (!state || !cookie || !matches(state, cookie)) return c.text('Invalid installation state', 400)
    if (c.req.query('error')) return c.text('Slack installation was not approved. Reopen your installation link to try again.', 400)
    const code = c.req.query('code')
    if (!code) return c.text('Missing authorization code', 400)
    const record = await config.store.claim(id, digest(state))
    if (!record) return c.text('Installation expired or already used', 400)
    deleteCookie(c, `crew_${id}`, { path: `/api/slack/crew/${id}` })
    const installed = await slack('oauth.v2.access', {
      client_id: record.clientId, client_secret: record.clientSecret, code,
      redirect_uri: `${base}/api/slack/crew/${id}/oauth`
    })
    if (installed.app_id !== record.appId || installed.team?.id !== config.teamId
        || installed.token_type !== 'bot' || !installed.access_token || !installed.bot_user_id
        || !CREW_BOT_SCOPES.every(scope => String(installed.scope).split(',').includes(scope))) {
      throw new Error('Slack installation identity or scopes did not match the requested Crew')
    }
    Object.assign(record, {
      status: 'active', botToken: installed.access_token,
      botUserId: installed.bot_user_id, teamId: installed.team.id
    })
    delete record.installTicket
    await config.store.save(record)
    await bot(record)
    config.botOptions.logger?.info('slackbotv2_crew_app_installed', {
      crew_id: record.id, app_id: record.appId, team_id: record.teamId, bot_user_id: record.botUserId
    })
    return c.text(`${record.name} is installed. Invite it to a channel or open its Slack DM.`)
  })

  app.post('/api/slack/crew/:id/events', async c => {
    const record = await config.store.get(c.req.param('id'))
    if (!record?.signingSecret) return c.text('Unknown Crew app', 404)
    const timestamp = c.req.header('x-slack-request-timestamp') ?? ''
    const body = await c.req.raw.clone().text()
    const expected = `v0=${createHmac('sha256', record.signingSecret).update(`v0:${timestamp}:${body}`).digest('hex')}`
    if (!/^\d+$/.test(timestamp) || Math.abs(Date.now() / 1000 - Number(timestamp)) > 300
        || !matches(c.req.header('x-slack-signature') ?? '', expected)) return c.text('Invalid Slack signature', 401)
    const payload = c.req.header('Content-Type')?.includes('application/json') ? JSON.parse(body) : undefined
    if (payload?.type === 'url_verification' && typeof payload.challenge === 'string') {
      return c.json({ challenge: payload.challenge })
    }
    if (payload && (payload.api_app_id !== record.appId || payload.team_id !== config.teamId)) {
      return c.text('Wrong Slack app or workspace', 403)
    }
    if (record.status !== 'active') return c.text('Installation is pending', 503)
    const instance = await bot(record)
    // Keep the signed body byte-for-byte intact. The existing ingress verifies the
    // app-specific signing secret before invoking any event callbacks.
    const url = new URL(c.req.url)
    url.pathname = '/api/webhooks/slack'
    let executionCtx
    try { executionCtx = c.executionCtx } catch { /* Bun has no execution context. */ }
    return instance.app.fetch(new Request(url, c.req.raw), c.env, executionCtx)
  })

  return {
    app,
    async restore() {
      for (const record of await config.store.list()) {
        if (record.status === 'active' && config.allowedPersonas.includes(record.personaId)) await bot(record)
      }
    }
  }
}
