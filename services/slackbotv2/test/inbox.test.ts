import { afterAll, describe, expect, it } from 'bun:test'
import pg from 'pg'
import {
  createMemorySlackInboxStore,
  createPostgresSlackInboxStore,
  type SlackInboxStore
} from '../src/inbox'

const postgresUrl = process.env.SLACKBOTV2_TEST_DATABASE_URL
const pool = postgresUrl ? new pg.Pool({ connectionString: postgresUrl }) : undefined

afterAll(async () => {
  await pool?.end()
})

const stores: Array<[string, (() => SlackInboxStore) | undefined]> = [
  ['memory', createMemorySlackInboxStore],
  [
    'postgres',
    pool ? () => createPostgresSlackInboxStore(pool, `test-${crypto.randomUUID()}`) : undefined
  ]
]

for (const [name, create] of stores) {
  describe.skipIf(!create)(`${name} Slack inbox store`, () => {
    it('keeps the first save of an event and lists pending bodies oldest first', async () => {
      const store = create!()
      const at = (seconds: number) => new Date(Date.UTC(2030, 0, 1, 0, 0, seconds))

      expect(await store.save('Ev2', 'second', at(2))).toBe(true)
      expect(await store.save('Ev1', 'first', at(1))).toBe(true)
      expect(await store.save('Ev1', 'redelivered', at(3))).toBe(false)
      expect(await store.save('Ev3', 'too new', at(4))).toBe(true)

      expect(await store.pending(at(0), at(4))).toEqual([
        { body: 'first', eventId: 'Ev1' },
        { body: 'second', eventId: 'Ev2' }
      ])
      expect(await store.pending(at(2), at(4))).toEqual([{ body: 'second', eventId: 'Ev2' }])

      await store.delete('Ev2')
      expect(await store.pending(at(0), at(5))).toEqual([{ body: 'too new', eventId: 'Ev3' }])
    })
  })
}
