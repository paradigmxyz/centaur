// API clients: machine callers of the session API, such as eval runners. An
// operator creates one in the console; it exchanges its secret for an access
// token, then runs sessions as its attached principal, only under its own
// api:<client id>: thread keys, until the operator disables it.
import { randomUUID } from 'node:crypto'
import { expect, test } from 'bun:test'
import { apiClients, ironControl, model, turnTimeoutMs } from '../lib'

const sessionPath = (key: string) => `/api/session/${encodeURIComponent(key)}`

test('an API client runs a turn as its principal, only in its own namespace', async () => {
  const client = await apiClients.create()
  const token = await apiClients.token(client)
  expect(token.status).toBe(200)
  expect(token.body).toMatchObject({ token_type: 'Bearer', scope: 'sessions:read sessions:write' })
  const session = apiClients.as(token.body.access_token)

  const key = `api:${client.clientId}:${randomUUID()}`
  const created = await session.ok('POST', sessionPath(key), { harness_type: 'codex' })
  expect(created.iron_control_principal).toBe(client.principalId)

  for (const other of [`api:acl_other:${randomUUID()}`, `e2e-api:${randomUUID()}`]) {
    const { status } = await session.request('POST', sessionPath(other), { harness_type: 'codex' })
    expect({ other, status }).toEqual({ other, status: 403 })
  }
  expect((await session.request('POST', '/api/sandboxes/drain')).status).toBe(403)

  const script = model.says('Hello from the API client.')
  await model.register(script)
  const text = `Say hello. ${script.token}`
  await session.ok('POST', `${sessionPath(key)}/messages`, {
    messages: [{ role: 'user', parts: [{ type: 'text', text }] }]
  })
  const execution = await session.ok('POST', `${sessionPath(key)}/execute`, {
    input_lines: [JSON.stringify({ type: 'user', message: { content: [{ type: 'text', text }] } })]
  })
  const events = await session.events(key, execution.execution_id)

  expect(events.at(-1)?.event).toBe('session.execution_completed')
  const ready = events.find(event => event.event === 'session.sandbox_ready')
  if (!ready) throw new Error(`the turn recorded no ready sandbox: ${JSON.stringify(events)}`)
  const proxy = await ironControl.proxy(JSON.parse(ready.data).sandbox_id)
  expect(proxy.principal.foreignId).toBe(client.principalForeignId)
  expect(proxy.requester).toBeNull()
  expect(await model.requests(script)).toHaveLength(1)

  await apiClients.disable(client)
  expect((await session.request('GET', sessionPath(key))).status).toBe(401)
  expect((await apiClients.token(client)).status).toBe(401)
}, turnTimeoutMs + 120_000)

test('an API client token carries only the scopes it asked for', async () => {
  const client = await apiClients.create({ scopes: ['sessions:read'] })
  expect((await apiClients.token(client, 'sessions:write')).body.error).toBe('invalid_scope')

  const token = await apiClients.token(client)
  expect(token.body.scope).toBe('sessions:read')
  const session = apiClients.as(token.body.access_token)
  const { status } = await session.request('POST', sessionPath(`api:${client.clientId}:${randomUUID()}`), {
    harness_type: 'codex'
  })
  expect(status).toBe(403)

  const wrongSecret = await apiClients.token({ ...client, secret: 'acs_wrong' })
  expect(wrongSecret).toEqual({
    status: 401,
    body: { error: 'invalid_client', error_description: 'client authentication failed' }
  })
}, 120_000)
