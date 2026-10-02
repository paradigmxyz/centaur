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
  async mutate(selector: { id: string } | { appId: string }, update: (record: CrewRecord) => boolean | Promise<boolean>) {
    const record = 'id' in selector ? this.records.get(selector.id)
      : [...this.records.values()].find(candidate => candidate.appId === selector.appId)
    if (!record) return undefined
    const current = structuredClone(record)
    if (!await update(current)) return undefined
    this.records.set(current.id, structuredClone(current))
    return current
  }
}

function fixture(overrides: { failCreate?: boolean; team?: string; appId?: string; rejectInstall?: boolean; invalidInstall?: boolean; iconError?: string; onInstall?: () => Promise<void> } = {}) {
  const store = new MemoryStore()
  const calls: string[] = []
  const slackBodies: Record<string, (URLSearchParams | Record<string, unknown>)[]> = {}
  const botOptions: SlackbotV2Options[] = []
  let deliveries = 0
  const config = {
    store, publicUrl: 'https://bots.example.com', adminToken: 'management-test',
    configurationToken: 'configuration-test', teamId: 'T123',
    botOptions: { apiUrl: 'http://api', botToken: 'primary-test', signingSecret: 'primary-signing' },
    fetch: (async (url: RequestInfo | URL, init?: RequestInit) => {
      const method = String(url).split('/').pop()!
      calls.push(method)
      const contentType = (init?.headers as Record<string, string>)['Content-Type']
      ;(slackBodies[method] ??= []).push(contentType?.startsWith('application/json')
        ? JSON.parse(init?.body as string) : new URLSearchParams(init?.body as string))
      if (method === 'apps.manifest.create') {
        expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer configuration-test')
        if (overrides.failCreate) throw new Error('Lost response')
        return Response.json({ ok: true, app_id: 'A123', credentials: {
          client_id: 'client-test', client_secret: 'client-secret-test', signing_secret: 'signing-test'
        } })
      }
      if (method === 'apps.manifest.update') return Response.json({ ok: true })
      if (method === 'apps.icon.set') {
        expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer configuration-test')
        return Response.json(overrides.iconError ? { ok: false, error: overrides.iconError } : { ok: true })
      }
      if (method === 'apps.developerInstall') {
        expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer configuration-test')
        const body = JSON.parse(init?.body as string)
        expect(body).toEqual({ app_id: 'A123', bot_scopes: [...CREW_BOT_SCOPES] })
        await overrides.onInstall?.()
        if (overrides.rejectInstall) return Response.json({ ok: false, error: 'not_allowed' })
        return Response.json(overrides.invalidInstall ? { ok: true } : {
          ok: true, team_id: overrides.team ?? 'T123', api_access_tokens: { bot: 'installed-test' }
        })
      }
      expect(method).toBe('auth.test')
      expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer installed-test')
      return Response.json({ ok: true, app_id: overrides.appId ?? 'A123', team_id: overrides.team ?? 'T123', user_id: 'UBOT' })
    }) as typeof fetch,
    createBot: (options: SlackbotV2Options) => {
      botOptions.push(options)
      const app = new Hono()
      app.post('/api/webhooks/slack', c => { deliveries++; return c.text('delivered') })
      return { app } as SlackbotV2
    }
  }
  const manager = createCrewManager(config)
  const create = (body = { id: 'research', name: 'Research' }, token = 'management-test') =>
    manager.app.request('/api/slack/crew', { method: 'POST', headers: {
      Authorization: `Bearer ${token}`, 'Content-Type': 'application/json'
    }, body: JSON.stringify(body) })
  return { ...manager, config, store, calls, slackBodies, botOptions, create, deliveries: () => deliveries }
}

async function install(f: ReturnType<typeof fixture>) {
  return f.create()
}

function event(secret: string, extra = {}, timestamp = String(Math.floor(Date.now() / 1000))) {
  const body = JSON.stringify({ type: 'event_callback', api_app_id: 'A123', team_id: 'T123', ...extra })
  return { method: 'POST', body, headers: { 'Content-Type': 'application/json',
    'x-slack-request-timestamp': timestamp,
    'x-slack-signature': `v0=${createHmac('sha256', secret).update(`v0:${timestamp}:${body}`).digest('hex')}` } }
}

describe('Crew provisioning', () => {
  test('requires management auth and accepts no profile registry dependency', async () => {
    const f = fixture()
    expect((await f.create(undefined, 'wrong')).status).toBe(401)
    expect((await f.app.request('/api/slack/crew')).status).toBe(401)
    expect((await f.create({ id: 'research', name: ' ' })).status).toBe(400)
    expect(f.calls).toEqual([])
  })

  test('lists Crew and manages only validated mutable fields with preserved manifest configuration', async () => {
    const f = fixture()
    await install(f)
    const list = await (await f.app.request('/api/slack/crew', { headers: { Authorization: 'Bearer management-test' } })).json()
    expect(list.crew[0]).toMatchObject({ description: 'A member of your Centaur crew', paused: false })
    expect(list).not.toHaveProperty('profiles')
    expect(list.crew[0]).not.toHaveProperty('crew_id')
    const path = '/api/slack/crew/research/manage'
    expect((await f.app.request(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' })).status).toBe(401)
    for (const body of [{ crew_id: 'legal' }, { name: ' ' }, { name: 'x'.repeat(36) },
      { description: 'x'.repeat(141) }, { paused: 'yes' }]) {
      expect((await f.app.request(path, { method: 'POST', headers: { Authorization: 'Bearer management-test',
        'Content-Type': 'application/json' }, body: JSON.stringify(body) })).status).toBe(400)
    }
    const response = await f.app.request(path, { method: 'POST', headers: { Authorization: 'Bearer management-test',
      'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'New Research', description: 'New description', paused: true }) })
    expect(response.status).toBe(200)
    expect(await response.json()).toMatchObject({ id: 'research', name: 'New Research', description: 'New description', paused: true })
    const updateCall = f.calls.lastIndexOf('apps.manifest.update')
    expect(updateCall).toBeGreaterThan(-1)
    const manifest = JSON.parse((f.slackBodies['apps.manifest.update']![0] as URLSearchParams).get('manifest')!)
    expect(manifest.display_information).toEqual({ name: 'New Research', description: 'New description' })
    expect(manifest.oauth_config.scopes.bot).toEqual(CREW_BOT_SCOPES)
    expect('redirect_urls' in manifest.oauth_config).toBe(false)
    expect(manifest.settings.event_subscriptions.request_url).toBe('https://bots.example.com/api/slack/crew/research/events')
  })

  test('by-app management is authenticated, identity-bound, active, and cannot pause', async () => {
    const f = fixture()
    await install(f)
    const path = '/api/slack/crew/by-app/A123/manage'
    expect((await f.app.request(path)).status).toBe(401)
    expect((await f.app.request('/api/slack/crew/by-app/AOTHER/manage', { headers: { Authorization: 'Bearer management-test' } })).status).toBe(404)
    expect((await f.app.request(path, { method: 'POST', headers: { Authorization: 'Bearer management-test',
      'Content-Type': 'application/json' }, body: JSON.stringify({ paused: true }) })).status).toBe(400)
    const response = await f.app.request(path, { method: 'POST', headers: { Authorization: 'Bearer management-test',
      'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'Bound app' }) })
    expect(response.status).toBe(200)
    expect(await response.json()).toMatchObject({ id: 'research', app_id: 'A123', name: 'Bound app', paused: false })
  })

  for (const path of ['/api/slack/crew/research/manage', '/api/slack/crew/by-app/A123/manage']) {
    test(`sets and remembers only the selected app icon through ${path}`, async () => {
      const f = fixture()
      await install(f)
      await f.store.save({ id: 'other', name: 'Other', appId: 'AOTHER', status: 'active', iconUrl: 'https://images.example.com/other.png' })
      const icon_url = 'https://images.example.com/research.png'
      const request = { method: 'POST', headers: { Authorization: 'Bearer management-test',
        'Content-Type': 'application/json' }, body: JSON.stringify({ icon_url }) }
      const response = await f.app.request(path, request)
      expect(response.status).toBe(200)
      expect(await response.json()).toMatchObject({ app_id: 'A123', icon_url })
      expect(Object.fromEntries(f.slackBodies['apps.icon.set']![0] as URLSearchParams))
        .toEqual({ app_id: 'A123', url: icon_url })
      expect((await f.store.get('research'))?.iconUrl).toBe(icon_url)
      expect((await f.store.get('other'))?.iconUrl).toBe('https://images.example.com/other.png')
      expect((await f.app.request(path, request)).status).toBe(200)
      expect(f.calls).toEqual(['apps.manifest.create', 'apps.developerInstall', 'auth.test', 'apps.icon.set'])
      const restarted = createCrewManager(f.config)
      const list = await (await restarted.app.request('/api/slack/crew', { headers: request.headers })).json()
      expect(list.crew.find((record: { id: string }) => record.id === 'research').icon_url).toBe(icon_url)
    })
  }

  test('rejects invalid picture URLs before Slack and retains the previous icon on Slack rejection', async () => {
    const f = fixture({ iconError: 'invalid_icon_size' })
    await install(f)
    const previous = 'https://images.example.com/previous.png'
    await f.store.save({ ...(await f.store.get('research'))!, iconUrl: previous })
    const path = '/api/slack/crew/research/manage'
    const headers = { Authorization: 'Bearer management-test', 'Content-Type': 'application/json' }
    for (const icon_url of ['', null, {}, 'not a URL', 'http://images.example.com/bot.png',
      'https://user:password@images.example.com/bot.png', 'data:image/png;base64,abc', `https://example.com/${'x'.repeat(2048)}`]) {
      expect((await f.app.request(path, { method: 'POST', headers, body: JSON.stringify({ icon_url }) })).status).toBe(400)
    }
    expect(f.calls).toHaveLength(3)
    const response = await f.app.request(path, { method: 'POST', headers,
      body: JSON.stringify({ icon_url: 'https://images.example.com/too-small.png' }) })
    expect(response.status).toBe(502)
    expect(await response.json()).toEqual({ error: 'Slack apps.icon.set: invalid_icon_size' })
    expect((await f.store.get('research'))?.iconUrl).toBe(previous)
    expect(f.slackBodies['apps.icon.set']).toHaveLength(1)
  })

  test('picture updates retain authentication and active self-management gates', async () => {
    const f = fixture()
    await install(f)
    const path = '/api/slack/crew/by-app/A123/manage'
    const request = { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ icon_url: 'https://images.example.com/bot.png' }) }
    expect((await f.app.request(path, request)).status).toBe(401)
    for (const state of [{ paused: true }, { paused: false, status: 'needs_install' as const }]) {
      await f.store.save({ ...(await f.store.get('research'))!, ...state })
      expect((await f.app.request(path, { ...request,
        headers: { ...request.headers, Authorization: 'Bearer management-test' } })).status).toBe(409)
    }
    expect(f.calls).toHaveLength(3)
  })

  test('durably deduplicates creation, checks conflicting definitions, and redacts credentials', async () => {
    const f = fixture()
    const responses = await Promise.all([f.create(), f.create()])
    expect(responses.some(r => r.status === 201)).toBe(true)
    expect(f.calls).toEqual(['apps.manifest.create', 'apps.developerInstall', 'auth.test'])
    const repeat = await f.create()
    expect(repeat.status).toBe(200)
    const data = await repeat.json()
    expect(data.status).toBe('active')
    expect(data.team_id).toBe('T123')
    expect(JSON.stringify(data)).not.toContain('secret')
    expect(JSON.stringify(data)).not.toContain('configuration-test')
    expect((await f.create({ id: 'research', name: 'Different' })).status).toBe(409)
    expect((await f.app.request('/api/slack/crew', { method: 'POST',
      headers: { Authorization: 'Bearer management-test', 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: 'research', name: 'Research', crew_id: 'research', description: 'Different definition' })
    })).status).toBe(409)
    expect(f.calls).toHaveLength(3)
  })

  test('does not retry app creation after an ambiguous Slack failure', async () => {
    const f = fixture({ failCreate: true })
    expect((await f.create()).status).toBe(502)
    expect((await f.create()).status).toBe(409)
    expect(f.calls).toHaveLength(1)
    expect((await f.store.get('research'))?.status).toBe('creating')
  })

  test('automatically installs once and authenticated install is idempotent', async () => {
    const f = fixture()
    expect((await install(f)).status).toBe(201)
    const path = '/api/slack/crew/research/install'
    expect((await f.app.request(path, { method: 'POST' })).status).toBe(401)
    expect((await f.app.request(path, { method: 'POST', headers: { Authorization: 'Bearer management-test' } })).status).toBe(200)
    expect(f.calls).toEqual(['apps.manifest.create', 'apps.developerInstall', 'auth.test'])
    expect(f.botOptions[0]).toMatchObject({ botAppId: 'A123', crewBot: true,
      botToken: 'installed-test', slackHomeTeamId: 'T123', stateKeyPrefix: 'centaur-slackbotv2:crew:A123' })
    expect(f.botOptions[0]).not.toHaveProperty('fixedPersonaId')
  })

  test('keeps explicit developer-install rejection recoverable without activating', async () => {
    const f = fixture({ rejectInstall: true })
    const response = await f.create()
    expect(response.status).toBe(502)
    expect(await response.json()).toMatchObject({ status: 'needs_install', app_id: 'A123', team_id: 'T123' })
    expect((await f.store.get('research'))?.botToken).toBeUndefined()
    expect(f.botOptions).toHaveLength(0)
  })

  test('commits installing before contacting Slack and cannot replay after final persistence fails', async () => {
    const f = fixture({ onInstall: async () => {
      expect((await f.store.get('research'))?.status).toBe('installing')
      const concurrent = await f.app.request('/api/slack/crew/research/install', {
        method: 'POST', headers: { Authorization: 'Bearer management-test' }
      })
      expect(concurrent.status).toBe(502)
    } })
    const mutate = f.store.mutate.bind(f.store)
    f.store.mutate = async (selector, update) => {
      if (f.calls.includes('auth.test')) throw new Error('Database unavailable after installation')
      return mutate(selector, update)
    }
    expect((await f.create()).status).toBe(502)
    f.store.mutate = mutate
    expect((await f.store.get('research'))?.status).toBe('installing')
    expect((await f.app.request('/api/slack/crew/research/install', {
      method: 'POST', headers: { Authorization: 'Bearer management-test' }
    })).status).toBe(502)
    expect(f.calls.filter(method => method === 'apps.developerInstall')).toHaveLength(1)
    expect(f.botOptions).toHaveLength(0)
  })

  for (const overrides of [{ team: 'TOTHER' }, { appId: 'AOTHER' }]) {
    test(`rejects an installation for the wrong identity ${JSON.stringify(overrides)}`, async () => {
      const f = fixture(overrides)
      expect((await install(f)).status).toBe(502)
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

  test('acknowledges verified events without delivery while paused and resumes delivery', async () => {
    const f = fixture()
    await install(f)
    const manage = (paused: boolean) => f.app.request('/api/slack/crew/research/manage', { method: 'POST',
      headers: { Authorization: 'Bearer management-test', 'Content-Type': 'application/json' },
      body: JSON.stringify({ paused }) })
    expect((await manage(true)).status).toBe(200)
    const path = '/api/slack/crew/research/events'
    expect((await f.app.request(path, event('signing-test'))).status).toBe(200)
    expect(f.deliveries()).toBe(0)
    expect((await f.app.request(path, event('wrong'))).status).toBe(401)
    expect((await manage(false)).status).toBe(200)
    expect((await f.app.request(path, event('signing-test'))).status).toBe(200)
    expect(f.deliveries()).toBe(1)
  })

  test('answers signed URL challenges and restores active bots', async () => {
    const f = fixture()
    await f.create()
    const challenge = await f.app.request('/api/slack/crew/research/events', event('signing-test', {
      type: 'url_verification', challenge: 'challenge-test'
    }))
    expect(await challenge.json()).toEqual({ challenge: 'challenge-test' })
    expect(f.botOptions).toHaveLength(1)
    const stored = (await f.store.get('research'))!
    stored.personaId = 'legacy-profile'
    await f.store.save(stored)
    const restarted = createCrewManager(f.config)
    await restarted.restore()
    expect(f.botOptions).toHaveLength(2)
    expect(f.botOptions[1]).toMatchObject({ crewBot: true })
  })

  test('omits blank optional descriptions in create and update manifests', async () => {
    const f = fixture()
    const headers = { Authorization: 'Bearer management-test', 'Content-Type': 'application/json' }
    const created = await f.app.request('/api/slack/crew', { method: 'POST', headers,
      body: JSON.stringify({ id: 'research', name: 'Research', crew_id: 'research', description: '' }) })
    expect(created.status).toBe(201)
    const createManifest = JSON.parse((f.slackBodies['apps.manifest.create']![0] as URLSearchParams).get('manifest')!)
    expect(createManifest.display_information).toEqual({ name: 'Research' })
    for (const description of ['Useful description', '', '   ']) {
      const updated = await f.app.request('/api/slack/crew/research/manage', { method: 'POST', headers,
        body: JSON.stringify({ description }) })
      expect(updated.status).toBe(200)
      const manifest = JSON.parse((f.slackBodies['apps.manifest.update']!.at(-1) as URLSearchParams).get('manifest')!)
      expect(manifest.display_information).toEqual(description === 'Useful description'
        ? { name: 'Research', description: 'Useful description' } : { name: 'Research' })
    }
    expect(crewManifest('Research', 'https://bots.example.com', 'research').display_information)
      .toEqual({ name: 'Research', description: 'A member of your Centaur crew' })
  })

  test('manifest includes mentions and DMs, with no browser callback or administration scopes', () => {
    const manifest = crewManifest('Research', 'https://bots.example.com', 'research')
    expect('redirect_urls' in manifest.oauth_config).toBe(false)
    expect(manifest.settings.event_subscriptions.bot_events).toContain('message.im')
    expect(manifest.settings.event_subscriptions.bot_events).toContain('app_mention')
    expect(CREW_BOT_SCOPES.some(scope => scope.startsWith('admin:'))).toBe(false)
  })
})

describe('Crew history', () => {
  test('uses only the selected active bot and accepts only authenticated read operations', async () => {
    const f = fixture()
    await install(f)
    f.store.records.set('other', { ...f.store.records.get('research')!, id: 'other', appId: 'A456', botToken: 'other-test' })
    const calls: string[] = []
    const { app } = createCrewManager({ ...f.config, fetch: (async (url, init) => {
      expect((init!.headers as Record<string, string>).Authorization).toBe('Bearer installed-test')
      const method = String(url).split('/').pop()!
      calls.push(method)
      const form = new URLSearchParams(init!.body as string)
      expect(form.get('channel')).toBe('C123456789')
      expect(form.get('cursor')).toBe('next-page')
      expect(form.get('limit')).toBe('3')
      if (method === 'conversations.replies') expect(form.get('ts')).toBe('1700000000.000001')
      return Response.json({ ok: true, messages: [{ ts: '1700000000.000001', text: 'Crew result' }] })
    }) as typeof fetch })
    const path = '/api/slack/crew/by-principal/slack-crew-t123-a123/history'
    const input = { method: 'conversations.replies', explicitly_allowed: true,
      parameters: { channel: 'C123456789', ts: '1700000000.000001', limit: '3', cursor: 'next-page' } }
    const request = (body = input, token = 'management-test', target = path) => app.request(target, {
      method: 'POST', headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }, body: JSON.stringify(body)
    })
    expect((await request(input, 'wrong')).status).toBe(401)
    expect((await request(input, 'management-test', path.replace('t123', 't999'))).status).toBe(404)
    expect((await request({ ...input, method: 'chat.postMessage' })).status).toBe(400)
    expect((await request({ ...input, parameters: { ...input.parameters, token: 'other-test' } } as typeof input)).status).toBe(400)
    expect(calls).toEqual([])
    for (const method of ['conversations.history', 'conversations.replies']) {
      const response = await request({ ...input, method })
      expect(response.status).toBe(200)
      expect(await response.json()).toEqual({ ok: true, messages: [{ ts: '1700000000.000001', text: 'Crew result' }] })
    }
    expect(calls).toEqual(['conversations.history', 'conversations.replies'])
    f.store.records.get('research')!.paused = true
    expect((await request()).status).toBe(404)
    f.store.records.get('research')!.paused = false
    f.store.records.get('research')!.status = 'needs_install'
    expect((await request()).status).toBe(404)
    expect(calls).toHaveLength(2)
  })

  test('ungranted channels require public membership of this bot, while explicit grants remain scoped', async () => {
    const f = fixture()
    await install(f)
    let reads = 0
    let channel: Record<string, unknown> = { id: 'C123456789', is_private: false, is_member: true }
    const { app } = createCrewManager({ ...f.config, fetch: (async (url, init) => {
      expect((init!.headers as Record<string, string>).Authorization).toBe('Bearer installed-test')
      if (String(url).endsWith('/conversations.info')) return Response.json({ ok: true, channel })
      reads++
      return Response.json({ ok: true, messages: [] })
    }) as typeof fetch })
    const request = (explicitly_allowed = false) => app.request('/api/slack/crew/by-principal/slack-crew-t123-a123/history', {
      method: 'POST', headers: { Authorization: 'Bearer management-test', 'Content-Type': 'application/json' },
      body: JSON.stringify({ method: 'conversations.history', explicitly_allowed, parameters: { channel: 'C123456789' } })
    })
    expect((await request()).status).toBe(200)
    for (const inaccessible of [
      { id: 'C123456789', is_private: true, is_member: true },
      { id: 'C123456789', is_private: false, is_member: false },
      { id: 'C123456789', is_member: true },
      { id: 'C987654321', is_private: false, is_member: true }
    ]) {
      channel = inaccessible
      expect((await request()).status).toBe(403)
    }
    expect(reads).toBe(1)
    channel = { id: 'C123456789', is_private: true, is_member: true }
    expect((await request(true)).status).toBe(200)
    expect(reads).toBe(2)
  })
})
