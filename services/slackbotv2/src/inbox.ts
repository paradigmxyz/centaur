import type pg from 'pg'

/** Raw Slack webhook bodies saved before Slack is acknowledged. */
export type SlackInboxStore = {
  /** Saves a body; returns false when the event is already saved. */
  save(eventId: string, body: string, acceptedAt: Date): Promise<boolean>
  delete(eventId: string): Promise<void>
  /** Drops bodies accepted before `since`, then lists those accepted before `before`, oldest first. */
  pending(since: Date, before: Date): Promise<Array<{ body: string; eventId: string }>>
}

const TABLE = 'slackbotv2_inbox'

export function createPostgresSlackInboxStore(pool: pg.Pool, keyPrefix: string): SlackInboxStore {
  let schema: Promise<unknown> | undefined
  const query = async <T extends pg.QueryResultRow>(text: string, values: unknown[]) => {
    schema ??= pool
      .query(
        `CREATE TABLE IF NOT EXISTS ${TABLE} (
          key_prefix text NOT NULL,
          event_id text NOT NULL,
          body text NOT NULL,
          accepted_at timestamptz NOT NULL,
          PRIMARY KEY (key_prefix, event_id)
        )`
      )
      .catch(error => {
        schema = undefined
        throw error
      })
    await schema
    return pool.query<T>(text, values)
  }

  return {
    async save(eventId, body, acceptedAt) {
      const result = await query(
        `INSERT INTO ${TABLE} (key_prefix, event_id, body, accepted_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (key_prefix, event_id) DO NOTHING`,
        [keyPrefix, eventId, body, acceptedAt]
      )
      return result.rowCount === 1
    },

    async delete(eventId) {
      await query(`DELETE FROM ${TABLE} WHERE key_prefix = $1 AND event_id = $2`, [
        keyPrefix,
        eventId
      ])
    },

    async pending(since, before) {
      await query(`DELETE FROM ${TABLE} WHERE key_prefix = $1 AND accepted_at < $2`, [
        keyPrefix,
        since
      ])
      const result = await query<{ body: string; event_id: string }>(
        `SELECT event_id, body FROM ${TABLE}
         WHERE key_prefix = $1 AND accepted_at < $2
         ORDER BY accepted_at, event_id`,
        [keyPrefix, before]
      )
      return result.rows.map(row => ({ body: row.body, eventId: row.event_id }))
    }
  }
}

/** Process-local store for callers that supply their own non-Postgres state. */
export function createMemorySlackInboxStore(): SlackInboxStore {
  const rows = new Map<string, { acceptedAt: Date; body: string }>()
  return {
    async save(eventId, body, acceptedAt) {
      if (rows.has(eventId)) return false
      rows.set(eventId, { acceptedAt, body })
      return true
    },
    async delete(eventId) {
      rows.delete(eventId)
    },
    async pending(since, before) {
      for (const [eventId, row] of rows) {
        if (row.acceptedAt < since) rows.delete(eventId)
      }
      return Array.from(rows, ([eventId, row]) => ({ eventId, ...row }))
        .filter(row => row.acceptedAt < before)
        .sort((a, b) => a.acceptedAt.getTime() - b.acceptedAt.getTime())
        .map(({ body, eventId }) => ({ body, eventId }))
    }
  }
}
