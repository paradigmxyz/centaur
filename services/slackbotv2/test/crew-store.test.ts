import { randomBytes } from 'node:crypto'
import { expect, test } from 'bun:test'
import { Pool } from 'pg'
import { PgCrewStore, type CrewRecord } from '../src/crew-store'

const databaseUrl = process.env.SLACK_CREW_TEST_DATABASE_URL

test.skipIf(!databaseUrl)('Postgres encrypts installations, restores them, and serializes concurrent OAuth claims', async () => {
  const schema = `crew_test_${randomBytes(8).toString('hex')}`
  const admin = new Pool({ connectionString: databaseUrl })
  await admin.query(`CREATE SCHEMA ${schema}`)
  const pool = new Pool({ connectionString: databaseUrl, options: `-c search_path=${schema}` })
  const key = randomBytes(32).toString('hex')
  try {
    const store = new PgCrewStore(pool, key)
    await store.initialize()
    const record: CrewRecord = { id: 'research', name: 'Research', personaId: 'eng', status: 'needs_install',
      clientSecret: 'synthetic-sensitive-secret', installTicket: 'ticket-test' }
    const reserved = await Promise.all([store.reserve(record), store.reserve(record)])
    expect(reserved.filter(Boolean)).toHaveLength(1)
    const raw = await pool.query('SELECT ciphertext FROM slackbotv2_crew')
    expect(raw.rows[0].ciphertext).not.toContain('synthetic-sensitive-secret')
    expect(await new PgCrewStore(pool, key).get('research')).toEqual(record)
    expect(await store.beginInstall('research', 'wrong', 'state')).toBeUndefined()
    await store.beginInstall('research', 'ticket-test', 'state-test')
    const claims = await Promise.all([store.claim('research', 'state-test'), store.claim('research', 'state-test')])
    expect(claims.filter(Boolean)).toHaveLength(1)
    expect((await store.get('research'))?.status).toBe('installing')
    expect(await store.beginInstall('research', 'ticket-test', 'new-state')).toBeUndefined()
    const installed = claims.find(Boolean)!
    installed.status = 'active'
    installed.botToken = 'synthetic-bot-token'
    await store.save(installed)
    let entered!: () => void
    const locked = new Promise<void>(resolve => { entered = resolve })
    let release!: () => void
    const proceed = new Promise<void>(resolve => { release = resolve })
    const rename = store.mutate({ id: 'research' }, async current => {
      entered()
      await proceed
      current.name = 'Renamed'
      return true
    })
    await locked
    // A second writer must read after the first commits, not replace its name
    // with a stale snapshot while pausing the app.
    let secondEntered = false
    const pause = store.mutate({ id: 'research' }, current => { secondEntered = true; current.paused = true; return true })
    await new Promise(resolve => setTimeout(resolve, 50))
    const readBeforeCommit = secondEntered
    release()
    await Promise.all([rename, pause])
    expect(readBeforeCommit).toBe(false)
    expect((await store.get('research'))?.botToken).toBe('synthetic-bot-token')
    expect((await store.get('research'))?.paused).toBe(true)
    expect((await store.get('research'))?.name).toBe('Renamed')
    expect((await new PgCrewStore(pool, key).list())[0]?.botToken).toBe('synthetic-bot-token')
    await expect(new PgCrewStore(pool, randomBytes(32).toString('hex')).get('research')).rejects.toThrow()
    await pool.query("UPDATE slackbotv2_crew SET id = 'tampered'")
    await expect(store.get('tampered')).rejects.toThrow()
  } finally {
    await pool.end()
    await admin.query(`DROP SCHEMA ${schema} CASCADE`)
    await admin.end()
  }
})
