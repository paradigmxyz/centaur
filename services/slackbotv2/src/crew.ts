import { createHash, createHmac, timingSafeEqual } from 'node:crypto'
import { Hono, type Context, type MiddlewareHandler } from 'hono'
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
  store: CrewStore
  botOptions: SlackbotV2Options
  fetch?: typeof globalThis.fetch
  createBot?: typeof createSlackbotV2
}

const DEFAULT_DESCRIPTION = 'A member of your Centaur crew'

export function crewManifest(name: string, base: string, id: string, description = DEFAULT_DESCRIPTION) {
  const events = `${base}/api/slack/crew/${id}/events`
  return {
    // Slack permits an omitted description, but rejects an empty one.
    display_information: { name, ...(description.trim() ? { description } : {}) },
    features: {
      bot_user: { display_name: name, always_online: true },
      app_home: { home_tab_enabled: false, messages_tab_enabled: true, messages_tab_read_only_enabled: false }
    },
    oauth_config: {
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

function validIconUrl(value: unknown): value is string {
  if (typeof value !== 'string' || value.length > 2048) return false
  try {
    const url = new URL(value)
    return url.protocol === 'https:' && !url.username && !url.password
  } catch {
    return false
  }
}

function publicRecord(record: CrewRecord) {
  return {
    id: record.id, name: record.name, description: record.description ?? DEFAULT_DESCRIPTION,
    paused: record.paused ?? false, status: record.status,
    app_id: record.appId, bot_user_id: record.botUserId, team_id: record.teamId,
    ...(record.iconUrl ? { icon_url: record.iconUrl } : {}),
    ...(record.installError ? { install_error: record.installError } : {})
  }
}

export function createCrewManager(config: CrewConfig) {
  const base = config.publicUrl.replace(/\/$/, '')
  const publicUrl = new URL(base)
  if (publicUrl.protocol !== 'https:' || publicUrl.username || publicUrl.password
      || publicUrl.search || publicUrl.hash || publicUrl.pathname !== '/') {
    throw new Error('SLACK_CREW_PUBLIC_URL must be an HTTPS origin')
  }
  if (!config.adminToken || !config.configurationToken || !/^T[A-Z0-9]+$/.test(config.teamId)) {
    throw new Error('Incomplete Crew configuration')
  }
  const app = new Hono()
  const bots = new Map<string, Promise<SlackbotV2>>()
  const fetchFn = config.fetch ?? globalThis.fetch

  async function slack(method: string, body: Record<string, unknown>, token?: string, json = false) {
    // Never retry side effects: ambiguous success requires reconciliation.
    const response = await fetchFn(`https://slack.com/api/${method}`, {
      method: 'POST',
      headers: {
        'Content-Type': json ? 'application/json; charset=utf-8' : 'application/x-www-form-urlencoded',
        ...(token ? { Authorization: `Bearer ${token}` } : {})
      },
      body: json ? JSON.stringify(body) : new URLSearchParams(Object.entries(body).map(([key, value]) => [key, String(value)])),
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
            || record.teamId !== config.teamId) {
          throw new Error('Crew installation is not active or allowed')
        }
        return (config.createBot ?? createSlackbotV2)({
          ...config.botOptions,
          botToken: record.botToken, botUserId: record.botUserId,
          botAppId: record.appId, signingSecret: record.signingSecret,
          crewBot: true, slackHomeTeamId: record.teamId,
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
  const requireAdmin: MiddlewareHandler = async (c, next) => {
    if (!matches(c.req.header('Authorization') ?? '', `Bearer ${config.adminToken}`)) {
      return c.json({ error: 'Unauthorized' }, 401)
    }
    return next()
  }
  app.use('/api/slack/crew', requireAdmin)
  app.use('/api/slack/crew/:id/install', requireAdmin)
  app.use('/api/slack/crew/:id/manage', requireAdmin)
  app.use('/api/slack/crew/by-app/:appId/manage', requireAdmin)
  app.use('/api/slack/crew/by-principal/:principal/history', requireAdmin)
  app.post('/api/slack/crew/by-principal/:principal/history', async c => {
    // api-rs resolves the signed JWT subject to this Console-owned identity.
    // Credentials stay here, and the request cannot select an arbitrary Slack API.
    const record = (await config.store.list()).find(candidate => candidate.appId
      && candidate.teamId === config.teamId
      && `slack-crew-${candidate.teamId.toLowerCase()}-${candidate.appId.toLowerCase()}` === c.req.param('principal'))
    if (!record || record.status !== 'active' || record.paused || !record.botToken) {
      return c.json({ error: 'Crew app not found or unavailable.' }, 404)
    }
    const input = await c.req.json().catch(() => null)
    const parameters = input?.parameters
    const allowed = new Set(['channel', 'cursor', 'latest', 'oldest', 'inclusive', 'include_all_metadata', 'limit', 'ts'])
    if (!input || !['conversations.history', 'conversations.replies'].includes(input.method)
        || typeof input.explicitly_allowed !== 'boolean'
        || !parameters || typeof parameters !== 'object' || Array.isArray(parameters)
        || Object.entries(parameters).some(([key, value]) => !allowed.has(key) || typeof value !== 'string')
        || !/^[CDG][A-Z0-9]{8,}$/.test(parameters.channel ?? '')
        || (input.method === 'conversations.replies' && !/^\d+\.\d+$/.test(parameters.ts ?? ''))
        || (parameters.limit !== undefined && (!/^\d+$/.test(parameters.limit)
          || Number(parameters.limit) < 1 || Number(parameters.limit) > 999))) {
      return c.json({ error: 'Invalid Crew history request.' }, 400)
    }
    if (!input.explicitly_allowed) {
      const info = await slack('conversations.info', { channel: parameters.channel }, record.botToken)
      if (info.channel?.id !== parameters.channel || info.channel?.is_private !== false
          || info.channel?.is_member !== true) {
        return c.json({ error: 'Not authorized to read history from this Slack channel.' }, 403)
      }
    }
    return c.json(await slack(input.method, parameters, record.botToken))
  })
  app.get('/api/slack/crew', async c => c.json({ crew: (await config.store.list()).map(publicRecord) }))
  app.post('/api/slack/crew', async c => {
    const input = await c.req.json().catch(() => null)
    if (!input || typeof input.id !== 'string' || !/^[a-z][a-z0-9-]{1,47}$/.test(input.id)
        || typeof input.name !== 'string' || input.name.trim().length < 1 || input.name.length > 35) {
      return c.json({ error: 'Provide id (2–48 lowercase slug characters) and name (1–35 characters).' }, 400)
    }
    if (input.description !== undefined && (typeof input.description !== 'string' || input.description.length > 140)) {
      return c.json({ error: 'Description must be at most 140 characters.' }, 400)
    }
    const record: CrewRecord = { id: input.id, name: input.name.trim(),
      description: input.description ?? DEFAULT_DESCRIPTION, paused: false,
      status: 'creating' }
    if (!await config.store.reserve(record)) {
      const existing = await config.store.get(record.id)
      if (!existing || existing.name !== record.name
          || (existing.description ?? DEFAULT_DESCRIPTION) !== record.description) {
        return c.json({ error: 'That id belongs to a different Crew definition.' }, 409)
      }
      // A lost create response cannot safely be retried at Slack. The durable reservation
      // remains visible for an operator to reconcile rather than spawning duplicates.
      return c.json(publicRecord(existing), existing.status === 'creating' ? 409 : 200)
    }
    const created = await slack('apps.manifest.create', {
      manifest: JSON.stringify(crewManifest(record.name, base, record.id, record.description))
    }, config.configurationToken)
    if (!/^A[A-Z0-9]+$/.test(created.app_id ?? '') || !created.credentials?.client_id
        || !created.credentials?.client_secret || !created.credentials?.signing_secret) {
      throw new Error('Incomplete Slack app creation response')
    }
    Object.assign(record, {
      appId: created.app_id, clientId: created.credentials.client_id,
      clientSecret: created.credentials.client_secret, signingSecret: created.credentials.signing_secret,
      teamId: config.teamId, status: 'needs_install'
    })
    await config.store.save(record)
    config.botOptions.logger?.info('slackbotv2_crew_app_created', {
      crew_id: record.id, app_id: record.appId
    })
    const installed = await install(record.id)
    return c.json(publicRecord(installed.record), installed.ok ? 201 : 502)
  })

  async function manage(c: Context, selector: { id: string } | { appId: string }, byApp: boolean) {
    const input = await c.req.json().catch(() => null)
    const allowed = byApp ? ['name', 'description', 'icon_url'] : ['name', 'description', 'icon_url', 'paused']
    if (!input || typeof input !== 'object' || Array.isArray(input)
        || Object.keys(input).some(key => !allowed.includes(key)) || Object.keys(input).length === 0
        || (input.name !== undefined && (typeof input.name !== 'string' || !input.name.trim() || input.name.length > 35))
        || (input.description !== undefined && (typeof input.description !== 'string' || input.description.length > 140))
        || (input.paused !== undefined && typeof input.paused !== 'boolean')) {
      return c.json({ error: 'Invalid Crew management fields.' }, 400)
    }
    if (input.icon_url !== undefined && !validIconUrl(input.icon_url)) {
      return c.json({ error: 'Profile picture must be a public HTTPS image URL of at most 2048 characters, without embedded credentials.' }, 400)
    }
    let blocked = false
    const updated = await config.store.mutate(selector, async record => {
      if ((byApp && (record.status !== 'active' || record.paused))
          || (!byApp && !['active', 'needs_install'].includes(record.status)) || !record.appId) {
        blocked = true
        return false
      }
      const name = input.name?.trim() ?? record.name
      const description = input.description ?? record.description ?? DEFAULT_DESCRIPTION
      if (input.name !== undefined || input.description !== undefined) {
        await slack('apps.manifest.update', {
          app_id: record.appId,
          manifest: JSON.stringify(crewManifest(name, base, record.id, description))
        }, config.configurationToken)
      }
      if (input.icon_url !== undefined && input.icon_url !== record.iconUrl) {
        // Slack fetches the public image; never fetch caller-supplied URLs here.
        await slack('apps.icon.set', { app_id: record.appId, url: input.icon_url }, config.configurationToken)
        record.iconUrl = input.icon_url
      }
      record.name = name
      record.description = description
      if (!byApp && input.paused !== undefined) record.paused = input.paused
      return true
    })
    if (!updated) return c.json({ error: blocked ? 'Crew app cannot be managed in its current state.' : 'Crew app not found.' }, blocked ? 409 : 404)
    return c.json(publicRecord(updated))
  }

  app.post('/api/slack/crew/:id/manage', c => manage(c, { id: c.req.param('id') }, false))
  app.get('/api/slack/crew/by-app/:appId/manage', async c => {
    const found = (await config.store.list()).find(record => record.appId === c.req.param('appId'))
    if (!found || found.status !== 'active' || found.paused) {
      return c.json({ error: 'Crew app not found or unavailable.' }, 404)
    }
    return c.json(publicRecord(found))
  })
  app.post('/api/slack/crew/by-app/:appId/manage', c => manage(c, { appId: c.req.param('appId') }, true))

  async function install(id: string): Promise<{ ok: boolean; record: CrewRecord }> {
    let claimed = false
    // Commit the claim BEFORE contacting Slack. A process crash or database
    // failure after a successful install must not make the operation replayable.
    const record = await config.store.mutate({ id }, async current => {
      if (current.status === 'active') return true
      if (current.status !== 'needs_install' || !current.appId
          || (current.teamId && current.teamId !== config.teamId)) return false
      current.teamId = config.teamId
      current.status = 'installing'
      current.installError = 'Slack installation is in progress; reconcile interrupted setup before retrying.'
      claimed = true
      return true
    })
    if (!record) throw new Error('Crew app is not available for installation')
    if (!claimed) return { ok: record.status === 'active', record }

    let token: string | undefined
    let botUserId: string | undefined
    let rejected = false
    let installError: string | undefined
    try {
      const installed = await slack('apps.developerInstall', {
        app_id: record.appId, bot_scopes: [...CREW_BOT_SCOPES]
      }, config.configurationToken, true)
      if (typeof installed.api_access_tokens?.bot !== 'string' || !installed.api_access_tokens.bot
          || (installed.team_id && installed.team_id !== config.teamId)
          || (installed.app_id && installed.app_id !== record.appId)) {
        throw new Error('Invalid installation identity')
      }
      token = installed.api_access_tokens.bot
      const identity = await slack('auth.test', {}, token)
      if (identity.team_id !== config.teamId || !/^U[A-Z0-9]+$/.test(identity.user_id ?? '')
          || (identity.app_id && identity.app_id !== record.appId)) {
        throw new Error('Invalid bot identity')
      }
      botUserId = identity.user_id
    } catch (error) {
      const message = error instanceof Error ? error.message : ''
      rejected = /^Slack apps\.developerInstall: /.test(message)
      installError = rejected ? message : 'Slack installation outcome is unknown; reconcile before retrying.'
    }
    const completed = await config.store.mutate({ id }, current => {
      if (current.status !== 'installing') return false
      current.status = botUserId ? 'active' : rejected ? 'needs_install' : 'installing'
      current.installError = installError
      if (token) current.botToken = token
      if (botUserId) current.botUserId = botUserId
      return true
    })
    if (!completed) throw new Error('Crew installation state changed')
    if (completed.status === 'active') {
      await bot(completed)
      config.botOptions.logger?.info('slackbotv2_crew_app_installed', {
        crew_id: completed.id, app_id: completed.appId, team_id: completed.teamId, bot_user_id: completed.botUserId
      })
    }
    return { ok: completed.status === 'active', record: completed }
  }

  app.post('/api/slack/crew/:id/install', async c => {
    const result = await install(c.req.param('id'))
    return c.json(publicRecord(result.record), result.ok ? 200 : 502)
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
    if (record.paused) return c.text('')
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
        if (record.status === 'active') await bot(record)
      }
    }
  }
}
