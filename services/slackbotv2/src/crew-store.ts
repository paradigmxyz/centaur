import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto'
import { Pool } from 'pg'

export type CrewRecord = {
  id: string
  name: string
  description?: string
  paused?: boolean
  personaId: string
  status: 'creating' | 'needs_install' | 'installing' | 'active'
  appId?: string
  clientId?: string
  clientSecret?: string
  signingSecret?: string
  botToken?: string
  botUserId?: string
  teamId?: string
  installTicket?: string
  oauthState?: string
  oauthExpiresAt?: number
}

export interface CrewStore {
  list(): Promise<CrewRecord[]>
  get(id: string): Promise<CrewRecord | undefined>
  reserve(record: CrewRecord): Promise<boolean>
  save(record: CrewRecord): Promise<void>
  /** Mutate the latest encrypted record while holding its database row lock. */
  mutate(selector: { id: string } | { appId: string }, update: (record: CrewRecord) => boolean | Promise<boolean>): Promise<CrewRecord | undefined>
  beginInstall(id: string, ticket: string, state: string): Promise<CrewRecord | undefined>
  /** Atomically claim an installation before exchanging its single-use code. */
  claim(id: string, state: string): Promise<CrewRecord | undefined>
}

/** App credentials never enter Chat SDK state, API metadata, or tool responses. */
export class PgCrewStore implements CrewStore {
  private readonly key: Buffer
  constructor(private readonly pool: Pool, encryptionKey: string) {
    if (!/^[a-f0-9]{64}$/i.test(encryptionKey)) {
      throw new Error('SLACK_CREW_ENCRYPTION_KEY must be 32 random bytes encoded as hex')
    }
    this.key = Buffer.from(encryptionKey, 'hex')
  }

  async initialize(): Promise<void> {
    await this.pool.query(`CREATE TABLE IF NOT EXISTS slackbotv2_crew (
      id text PRIMARY KEY, ciphertext text NOT NULL
    )`)
  }

  private encrypt(record: CrewRecord): string {
    const iv = randomBytes(12)
    const cipher = createCipheriv('aes-256-gcm', this.key, iv)
    cipher.setAAD(Buffer.from(record.id))
    const bytes = Buffer.concat([cipher.update(JSON.stringify(record)), cipher.final()])
    return Buffer.concat([iv, cipher.getAuthTag(), bytes]).toString('base64')
  }

  private decrypt(row: { id: string; ciphertext: string }): CrewRecord {
    const bytes = Buffer.from(row.ciphertext, 'base64')
    const decipher = createDecipheriv('aes-256-gcm', this.key, bytes.subarray(0, 12))
    decipher.setAAD(Buffer.from(row.id))
    decipher.setAuthTag(bytes.subarray(12, 28))
    return JSON.parse(Buffer.concat([decipher.update(bytes.subarray(28)), decipher.final()]).toString())
  }

  async list(): Promise<CrewRecord[]> {
    const result = await this.pool.query('SELECT id, ciphertext FROM slackbotv2_crew ORDER BY id')
    return result.rows.map(row => this.decrypt(row))
  }

  async get(id: string): Promise<CrewRecord | undefined> {
    const result = await this.pool.query('SELECT id, ciphertext FROM slackbotv2_crew WHERE id = $1', [id])
    return result.rows[0] ? this.decrypt(result.rows[0]) : undefined
  }

  async reserve(record: CrewRecord): Promise<boolean> {
    const result = await this.pool.query(
      'INSERT INTO slackbotv2_crew (id, ciphertext) VALUES ($1, $2) ON CONFLICT DO NOTHING',
      [record.id, this.encrypt(record)]
    )
    return result.rowCount === 1
  }

  async save(record: CrewRecord): Promise<void> {
    const result = await this.pool.query('UPDATE slackbotv2_crew SET ciphertext = $2 WHERE id = $1',
      [record.id, this.encrypt(record)])
    if (result.rowCount !== 1) throw new Error('Crew record is missing')
  }

  async claim(id: string, state: string): Promise<CrewRecord | undefined> {
    return this.mutate({ id }, record => {
      if (record.status !== 'needs_install') return false
      if (record.oauthState !== state || !record.oauthExpiresAt || record.oauthExpiresAt < Date.now()) return false
      record.status = 'installing'
      delete record.oauthState
      delete record.oauthExpiresAt
      return true
    })
  }

  async beginInstall(id: string, ticket: string, state: string): Promise<CrewRecord | undefined> {
    return this.mutate({ id }, record => {
      if (record.status !== 'needs_install') return false
      if (!ticket || record.installTicket !== ticket) return false
      record.oauthState = state
      record.oauthExpiresAt = Date.now() + 10 * 60_000
      return true
    })
  }

  async mutate(selector: { id: string } | { appId: string }, update: (record: CrewRecord) => boolean | Promise<boolean>): Promise<CrewRecord | undefined> {
    const client = await this.pool.connect()
    try {
      await client.query('BEGIN')
      const result = 'id' in selector
        ? await client.query('SELECT id, ciphertext FROM slackbotv2_crew WHERE id = $1 FOR UPDATE', [selector.id])
        : await client.query('SELECT id, ciphertext FROM slackbotv2_crew ORDER BY id FOR UPDATE')
      const records = result.rows.map(row => this.decrypt(row))
      const record = 'id' in selector ? records[0] : records.find(candidate => candidate.appId === selector.appId)
      if (!record || !await update(record)) {
        await client.query('ROLLBACK')
        return undefined
      }
      await client.query('UPDATE slackbotv2_crew SET ciphertext = $2 WHERE id = $1', [record.id, this.encrypt(record)])
      await client.query('COMMIT')
      return record
    } catch (error) {
      await client.query('ROLLBACK')
      throw error
    } finally {
      client.release()
    }
  }
}
