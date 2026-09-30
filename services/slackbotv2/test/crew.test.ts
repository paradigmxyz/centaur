import { createHmac } from 'node:crypto'
import { describe, expect, test } from 'bun:test'
import { Hono } from 'hono'
import { createCrewManager, CREW_BOT_SCOPES, crewManifest } from '../src/crew'
import type { CrewRecord, CrewStore } from '../src/crew-store'
import type { SlackbotV2, SlackbotV2Options } from '../src/types'

class MemoryStore implements CrewStore {
  records = new Map<string, CrewRecord>()
  async list() { return structuredClone([...this.records.values()]) }
  async get(id: string) { return structuredClone(this.records.get(id)) }
  async reserve(record: CrewRecord) {
    if (this.records.has(record.id)) return false
    this.records.set(record.id, structuredClone(record))
    return true
  }
  async save(record: CrewRecord) { this.records.set(record.id, structuredClone(record)) }
  async beginInstall(id: string, ticket: string, state: string) {
    const record = this.records.get(id)
    if (!record || record.status !== 'needs_install' || !ticket || record.installTicket !== ticket) return undefined
    record.oauthState = state
    record.oauthExpiresAt = Date.now() + 600_000
    return structuredClone(record)
  }
  async claim(id: string, state: string) {
    const record = this.records.get(id)
    if (!record || record.status !== 'needs_install' || record.oauthState !== state
        || !record.oauthExpiresAt || record.oauthExpiresAt < Date.now()) return undefined
    record.status = 'installing'
    delete record.oauthState
    return structuredClone(record)
  }
}

function fixture(overrides: { failCreate?: boolean; team?: string; appId?: string } = {}) {
  const store = new MemoryStore()
  const calls: string[] = []
  const botOptions: SlackbotV2Options[] = []
  let deliveries = 0
  const config = {
    store, publicUrl: 'https://bots.example.com', adminToken: 'management-test',
    configurationToken: 'configuration-test', teamId: 'T123', allowedPersonas: ['eng', 'legal'],
    botOptions: { apiUrl: 'http://api', botToken: 'primary-test', signingSecret: 'primary-signing' },
    fetch: (async (url: RequestInfo | URL, init?: RequestInit) => {
      const method = String(url).split('/').pop()!
      calls.push(method)
      if (method === 'apps.manifest.create') {
        expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer configuration-test')
        if (overrides.failCreate) throw new Error('Lost response')
        return Response.json({ ok: true, app_id: 'A123', credentials: {
          client_id: 'client-test', client_secret: 'client-secret-test', signing_secret: 'signing-test'
        } })
      }
      expect(method).toBe('oauth.v2.access')
      expect(new URLSearchParams(init?.body as string).get('client_secret')).toBe('client-secret-test')
      return Response.json({ ok: true, app_id: overrides.appId ?? 'A123', team: { id: overrides.team ?? 'T123' },
        token_type: 'bot', access_token: 'installed-test', bot_user_id: 'UBOT', scope: CREW_BOT_SCOPES.join(',') })
    }) as typeof fetch,
    createBot: (options: SlackbotV2Options) => {
      botOptions.push(options)
      const app = new Hono()
      app.post('/api/webhooks/slack', c => { deliveries++; return c.text('delivered') })
      return { app } as SlackbotV2
    }
  }
  const manager = createCrewManager(config)
  const create = (body = { id: 'research', name: 'Research', crew_id: 'eng' }, token = 'management-test') =>
    manager.app.request('/api/slack/crew', { method: 'POST', headers: {
      Authorization: `Bearer ${token}`, 'Content-Type': 'application/json'
    }, body: JSON.stringify(body) })
  return { ...manager, config, store, calls, botOptions, create, deliveries: () => deliveries }
}

async function install(f: ReturnType<typeof fixture>) {
  const created = await (await f.create()).json()
  const start = await f.app.request(created.install_url)
  const state = new URL(start.headers.get('Location')!).searchParams.get('state')!
  const cookie = start.headers.get('Set-Cookie')!.split(';')[0]!
  const path = `/api/slack/crew/research/oauth?code=approved-test&state=${state}`
  return { state, cookie, path, response: await f.app.request(path, { headers: { Cookie: cookie } }) }
}

function event(secret: string, extra = {}, timestamp = String(Math.floor(Date.now() / 1000))) {
  const body = JSON.stringify({ type: 'event_callback', api_app_id: 'A123', team_id: 'T123', ...extra })
  return { method: 'POST', body, headers: { 'Content-Type': 'application/json',
    'x-slack-request-timestamp': timestamp,
    'x-slack-signature': `v0=${createHmac('sha256', secret).update(`v0:${timestamp}:${body}`).digest('hex')}` } }
}

describe('Crew provisioning', () => {
  test('requires management auth and an allowed profile before calling Slack', async () => {
    const f = fixture()
    expect((await f.create(undefined, 'wrong')).status).toBe(401)
    expect((await f.app.request('/api/slack/crew')).status).toBe(401)
    expect((await f.create({ id: 'research', name: 'Research', crew_id: 'unknown' })).status).toBe(400)
    expect(f.calls).toEqual([])
  })

  test('durably deduplicates creation, checks conflicting definitions, and redacts credentials', async () => {
    const f = fixture()
    const responses = await Promise.all([f.create(), f.create()])
    expect(responses.some(r => r.status === 201)).toBe(true)
    expect(f.calls).toEqual(['apps.manifest.create'])
    const repeat = await f.create()
    expect(repeat.status).toBe(200)
    const data = await repeat.json()
    expect(data.status).toBe('needs_install')
    expect(data.install_url).toStartWith('https://bots.example.com/')
    expect(JSON.stringify(data)).not.toContain('secret')
    expect(JSON.stringify(data)).not.toContain('configuration-test')
    expect((await f.create({ id: 'research', name: 'Different', crew_id: 'eng' })).status).toBe(409)
    expect(f.calls).toHaveLength(1)
  })

  test('does not retry app creation after an ambiguous Slack failure', async () => {
    const f = fixture({ failCreate: true })
    expect((await f.create()).status).toBe(502)
    expect((await f.create()).status).toBe(409)
    expect(f.calls).toHaveLength(1)
    expect((await f.store.get('research'))?.status).toBe('creating')
  })

  test('requires the browser cookie, checks expiry, and prevents OAuth replay', async () => {
    const f = fixture()
    const result = await install(f)
    expect(result.response.status).toBe(200)
    expect((await f.app.request(result.path)).status).toBe(400)
    expect((await f.app.request(result.path, { headers: { Cookie: result.cookie } })).status).toBe(400)
    expect(f.calls).toEqual(['apps.manifest.create', 'oauth.v2.access'])
    expect(f.botOptions[0]).toMatchObject({ botAppId: 'A123', fixedPersonaId: 'eng',
      botToken: 'installed-test', slackHomeTeamId: 'T123', stateKeyPrefix: 'centaur-slackbotv2:crew:A123' })
    const expired = fixture()
    const created = await (await expired.create()).json()
    const start = await expired.app.request(created.install_url)
    const state = new URL(start.headers.get('Location')!).searchParams.get('state')!
    const record = (await expired.store.get('research'))!
    record.oauthExpiresAt = 1
    await expired.store.save(record)
    expect((await expired.app.request(`/api/slack/crew/research/oauth?code=x&state=${state}`, {
      headers: { Cookie: start.headers.get('Set-Cookie')!.split(';')[0]! }
    })).status).toBe(400)
    expect(expired.calls).toHaveLength(1)
  })

  for (const overrides of [{ team: 'TOTHER' }, { appId: 'AOTHER' }]) {
    test(`rejects an installation for the wrong identity ${JSON.stringify(overrides)}`, async () => {
      const f = fixture(overrides)
      expect((await install(f)).response.status).toBe(502)
      expect((await f.store.get('research'))?.status).toBe('installing')
      expect(f.botOptions).toHaveLength(0)
    })
  }

  test('verifies app-specific signatures, timestamps, and workspace before delivery', async () => {
    const f = fixture()
    await install(f)
    const path = '/api/slack/crew/research/events'
    expect((await f.app.request(path, event('primary-signing'))).status).toBe(401)
    expect((await f.app.request(path, event('signing-test', {}, '1'))).status).toBe(401)
    expect((await f.app.request(path, event('signing-test', { team_id: 'TOTHER' }))).status).toBe(403)
    expect(f.deliveries()).toBe(0)
    expect((await f.app.request(path, event('signing-test'))).status).toBe(200)
    expect(f.deliveries()).toBe(1)
  })

  test('answers signed URL challenges before installation and restores active bots', async () => {
    const f = fixture()
    await f.create()
    const challenge = await f.app.request('/api/slack/crew/research/events', event('signing-test', {
      type: 'url_verification', challenge: 'challenge-test'
    }))
    expect(await challenge.json()).toEqual({ challenge: 'challenge-test' })
    expect(f.botOptions).toHaveLength(0)
    await install(f)
    const restarted = createCrewManager(f.config)
    await restarted.restore()
    expect(f.botOptions).toHaveLength(2)
  })

  test('manifest includes install callback, mentions, DMs, and no administration scopes', () => {
    const manifest = crewManifest('Research', 'https://bots.example.com', 'research')
    expect(manifest.oauth_config.redirect_urls).toEqual(['https://bots.example.com/api/slack/crew/research/oauth'])
    expect(manifest.settings.event_subscriptions.bot_events).toContain('message.im')
    expect(manifest.settings.event_subscriptions.bot_events).toContain('app_mention')
    expect(CREW_BOT_SCOPES.some(scope => scope.startsWith('admin:'))).toBe(false)
  })
})
