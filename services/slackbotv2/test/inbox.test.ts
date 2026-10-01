import { createHmac } from 'node:crypto'
import { afterAll, describe, expect, it } from 'bun:test'
import pg from 'pg'
import {
  createMemorySlackInboxStore,
  createPostgresSlackInboxStore,
  createSlackInbox,
  type SlackInboxStore
} from '../src/inbox'
import { noopLogger } from '../src/utils'

const postgresUrl = process.env.SLACKBOTV2_TEST_DATABASE_URL
const pool = postgresUrl ? new pg.Pool({ connectionString: postgresUrl }) : undefined

afterAll(async () => {
  await pool?.end()
})

const stores: Array<[string, (() => Promise<SlackInboxStore>) | undefined]> = [
  ['memory', async () => createMemorySlackInboxStore()],
  [
    'postgres',
    pool
      ? async () => {
          await pool.query('DROP TABLE IF EXISTS slackbotv2_inbox')
          return createPostgresSlackInboxStore(pool, 'test')
        }
      : undefined
  ]
]

for (const [name, create] of stores) {
  describe.skipIf(!create)(`${name} Slack inbox store`, () => {
    it('lists saved requests oldest first and drops deleted and expired ones', async () => {
      const store = await create!()
      const at = (seconds: number) => new Date(Date.UTC(2030, 0, 1, 0, 0, seconds))

      const second = await store.save('second', at(2))
      const first = await store.save('first', at(1))
      const retry = await store.save('first', at(3))
      await store.save('too new', at(5))

      expect(await store.pending(at(0), at(5))).toEqual([
        { body: 'first', id: first },
        { body: 'second', id: second },
        { body: 'first', id: retry }
      ])

      await store.delete(retry)
      expect(await store.pending(at(2), at(5))).toEqual([{ body: 'second', id: second }])
      expect(await store.pending(at(0), at(5))).toEqual([{ body: 'second', id: second }])
    })
  })
}

describe.skipIf(!pool)('postgres Slack inbox store namespaces', () => {
  it('keeps deployments that share a database apart', async () => {
    const at = new Date(Date.UTC(2030, 0, 1))
    const ours = createPostgresSlackInboxStore(pool!, 'ours')
    const theirs = createPostgresSlackInboxStore(pool!, 'theirs')
    const id = await ours.save('ours', at)
    await theirs.save('theirs', at)

    expect(await ours.pending(new Date(0), new Date(at.getTime() + 1))).toEqual([
      { body: 'ours', id }
    ])
  })
})

describe('Slack inbox', () => {
  const signingSecret = 'inbox-signing-secret'
  const signedHeaders = (body: string) => {
    const timestamp = Math.floor(Date.now() / 1000).toString()
    const signature = createHmac('sha256', signingSecret)
      .update(`v0:${timestamp}:${body}`)
      .digest('hex')
    return new Headers({
      'x-slack-request-timestamp': timestamp,
      'x-slack-signature': `v0=${signature}`
    })
  }
  const createInbox = (store: SlackInboxStore, delivered: string[], retries: string[] = []) =>
    createSlackInbox({
      deliver: async request => {
        delivered.push(await request.text())
        retries.push(request.headers.get('x-slack-retry-num') ?? '')
        return new Response('ok')
      },
      logger: noopLogger,
      maxAgeMs: 60_000,
      replayDelayMs: 10,
      replayOnStart: true,
      saveTimeoutMs: 50,
      signingSecret,
      store
    })

  it('replays requests a previous process left, but not its own', async () => {
    const store = createMemorySlackInboxStore()
    await store.save('{"left":"behind"}', new Date())
    await Bun.sleep(2)
    const delivered: string[] = []
    const inbox = createInbox(store, delivered)
    // Saved but never delivered, as if this process were still working on it.
    const accepted = await inbox.accept('{"still":"running"}', signedHeaders('{"still":"running"}'))
    expect(accepted?.response.status).toBe(200)

    await Bun.sleep(50)
    expect(delivered).toEqual(['{"left":"behind"}'])
    expect(await store.pending(new Date(0), new Date(Date.now() + 1000))).toEqual([
      expect.objectContaining({ body: '{"still":"running"}' })
    ])

    await accepted?.deliver?.()
    expect(delivered).toEqual(['{"left":"behind"}', '{"still":"running"}'])
    expect(await store.pending(new Date(0), new Date(Date.now() + 1000))).toEqual([])
  })

  it('keeps Slack retry headers on live delivery', async () => {
    const delivered: string[] = []
    const retries: string[] = []
    const inbox = createInbox(createMemorySlackInboxStore(), delivered, retries)
    const headers = signedHeaders('{}')
    headers.set('x-slack-retry-num', '2')
    await (await inbox.accept('{}', headers))?.deliver?.()
    expect(retries).toEqual(['2'])
  })

  it('falls back to synchronous delivery when the save is slow, without leaving a row', async () => {
    const store = createMemorySlackInboxStore()
    let finishSave = () => {}
    const slowStore: SlackInboxStore = {
      ...store,
      save: (body, acceptedAt) =>
        new Promise(resolve => {
          finishSave = () => resolve(store.save(body, acceptedAt))
        })
    }
    const inbox = createInbox(slowStore, [])
    expect(await inbox.accept('{}', signedHeaders('{}'))).toBeNull()

    finishSave()
    await Bun.sleep(5)
    expect(await store.pending(new Date(0), new Date(Date.now() + 1000))).toEqual([])
  })

  it('rejects unsigned requests without saving them', async () => {
    const store = createMemorySlackInboxStore()
    const inbox = createInbox(store, [])
    const accepted = await inbox.accept('{}', new Headers())
    expect(accepted?.response.status).toBe(401)
    expect(accepted?.deliver).toBeUndefined()
    expect(await store.pending(new Date(0), new Date(Date.now() + 1000))).toEqual([])
  })
})
