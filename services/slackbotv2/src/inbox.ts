import { createHmac, randomUUID } from 'node:crypto'
import { verifySlackSignature } from '@chat-adapter/slack/webhook'
import type { Logger, WebhookOptions } from 'chat'
import type pg from 'pg'
import { errorMessage } from './utils'

/** Slack webhook requests saved before Slack is acknowledged. */
export type SlackInboxStore = {
  save(body: string, acceptedAt: Date): Promise<string>
  delete(id: string): Promise<void>
  /** Drops requests accepted before `since`, then lists those accepted before `before`, oldest first. */
  pending(since: Date, before: Date): Promise<Array<{ body: string; id: string }>>
}

const TABLE = 'slackbotv2_inbox'

/** `namespace` keeps deployments that share a database apart. */
export function createPostgresSlackInboxStore(pool: pg.Pool, namespace: string): SlackInboxStore {
  let schema: Promise<unknown> | undefined
  const query = async <T extends pg.QueryResultRow>(text: string, values: unknown[]) => {
    schema ??= pool
      .query(
        `CREATE TABLE IF NOT EXISTS ${TABLE} (
          namespace text NOT NULL,
          id text NOT NULL,
          body text NOT NULL,
          accepted_at timestamptz NOT NULL,
          PRIMARY KEY (namespace, id)
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
    async save(body, acceptedAt) {
      const id = randomUUID()
      await query(
        `INSERT INTO ${TABLE} (namespace, id, body, accepted_at) VALUES ($1, $2, $3, $4)`,
        [namespace, id, body, acceptedAt]
      )
      return id
    },
    async delete(id) {
      await query(`DELETE FROM ${TABLE} WHERE namespace = $1 AND id = $2`, [namespace, id])
    },
    async pending(since, before) {
      await query(`DELETE FROM ${TABLE} WHERE namespace = $1 AND accepted_at < $2`, [
        namespace,
        since
      ])
      const result = await query<{ body: string; id: string }>(
        `SELECT id, body FROM ${TABLE}
         WHERE namespace = $1 AND accepted_at < $2
         ORDER BY accepted_at, id`,
        [namespace, before]
      )
      return result.rows
    }
  }
}

/** Process-local store, for tests and deployments without Postgres. */
export function createMemorySlackInboxStore(): SlackInboxStore {
  const rows = new Map<string, { acceptedAt: Date; body: string }>()
  return {
    async save(body, acceptedAt) {
      const id = randomUUID()
      rows.set(id, { acceptedAt, body })
      return id
    },
    async delete(id) {
      rows.delete(id)
    },
    async pending(since, before) {
      for (const [id, row] of rows) {
        if (row.acceptedAt < since) rows.delete(id)
      }
      return Array.from(rows, ([id, row]) => ({ id, ...row }))
        .filter(row => row.acceptedAt < before)
        .sort((a, b) => a.acceptedAt.getTime() - b.acceptedAt.getTime())
        .map(({ body, id }) => ({ body, id }))
    }
  }
}

export type SlackInbox = ReturnType<typeof createSlackInbox>

/**
 * Holds verified Slack webhook requests for longer than Slack's 3-second
 * deadline and feeds them to the Chat SDK. A request is saved before Slack is
 * acknowledged and deleted once the Chat SDK's handlers finish; on startup,
 * requests a previous process left behind are fed again. It assumes one
 * replica whose previous process has exited before it starts (a Recreate
 * rollout), so every request saved before this process started is abandoned.
 */
export function createSlackInbox(deps: {
  /** Feeds a request to the Chat SDK, e.g. `chat.webhooks.slack`. */
  deliver: (request: Request, options: WebhookOptions) => Promise<Response>
  logger: Logger
  maxAgeMs: number
  replayDelayMs: number
  replayOnStart: boolean
  /** A save slower than this falls back to the synchronous path. */
  saveTimeoutMs: number
  signingSecret: string
  store: SlackInboxStore
}) {
  const { logger, store } = deps
  const startedAt = new Date()

  const deliver = async (id: string, body: string, retryHeaders?: Headers): Promise<void> => {
    try {
      const tasks: Promise<unknown>[] = []
      const request = signedSlackRequest(body, deps.signingSecret, retryHeaders)
      const response = await deps.deliver(request, {
        waitUntil: task => {
          tasks.push(task)
        }
      })
      if (!response.ok) throw new Error(`Chat SDK rejected the request with HTTP ${response.status}`)
      await Promise.all(tasks)
    } finally {
      // One delivery per request; a leftover row is dropped once it is too old.
      await store.delete(id).catch(() => undefined)
    }
  }

  const replay = async (): Promise<void> => {
    await sleep(deps.replayDelayMs)
    const deadlineMs = Date.now() + deps.maxAgeMs
    let pending: Array<{ body: string; id: string }>
    for (let attempt = 1; ; attempt += 1) {
      try {
        pending = await store.pending(new Date(Date.now() - deps.maxAgeMs), startedAt)
        break
      } catch (error) {
        logger.warn('slackbotv2_inbox_replay_scan_failed', { attempt, error: errorMessage(error) })
        // Every request is too old to answer by then.
        if (Date.now() >= deadlineMs) return
        await sleep(Math.min(250 * 2 ** attempt, 5_000))
      }
    }
    for (const { body, id } of pending) {
      logger.info('slackbotv2_inbox_replay_started', { inbox_id: id })
      try {
        await deliver(id, body)
        logger.info('slackbotv2_inbox_replay_complete', { inbox_id: id })
      } catch (error) {
        logger.warn('slackbotv2_inbox_replay_failed', { error: errorMessage(error), inbox_id: id })
      }
    }
  }

  if (deps.replayOnStart) void replay()

  return {
    /**
     * Verifies and saves a Slack webhook request. Returns the response for
     * Slack and the delivery to run in background, or null when the request
     * could not be saved and must be fed to the Chat SDK synchronously.
     */
    async accept(
      body: string,
      headers: Headers
    ): Promise<{ deliver?: () => Promise<void>; response: Response } | null> {
      try {
        await verifySlackSignature(body, headers, { signingSecret: deps.signingSecret })
      } catch (error) {
        logger.warn('slackbotv2_inbox_signature_rejected', { error: errorMessage(error) })
        return { response: new Response('Invalid signature', { status: 401 }) }
      }
      const saving = store.save(body, new Date())
      let id: string
      try {
        id = await withTimeout(saving, deps.saveTimeoutMs)
      } catch (error) {
        logger.warn('slackbotv2_inbox_save_failed', { error: errorMessage(error) })
        // The caller delivers synchronously; a late save must not be replayed.
        void saving.then(lateId => store.delete(lateId)).catch(() => undefined)
        return null
      }
      // Keep Slack's retry marker on live delivery so the Slack adapter can drop
      // a retry it already dispatched. Replay omits it: a replayed request must
      // be fed again even if the previous process had dispatched it.
      const retryHeaders = new Headers()
      for (const name of ['x-slack-retry-num', 'x-slack-retry-reason']) {
        const value = headers.get(name)
        if (value) retryHeaders.set(name, value)
      }
      return {
        deliver: () =>
          deliver(id, body, retryHeaders).catch(error => {
            logger.warn('slackbotv2_inbox_delivery_failed', {
              error: errorMessage(error),
              inbox_id: id
            })
          }),
        response: new Response('ok', { status: 200 })
      }
    }
  }
}

/** A freshly signed copy of a verified Slack webhook body. */
function signedSlackRequest(body: string, signingSecret: string, extraHeaders?: Headers): Request {
  const timestamp = Math.floor(Date.now() / 1000).toString()
  const signature = createHmac('sha256', signingSecret)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')
  const headers = new Headers(extraHeaders)
  headers.set('content-type', 'application/json')
  headers.set('x-slack-request-timestamp', timestamp)
  headers.set('x-slack-signature', `v0=${signature}`)
  return new Request('http://slackbotv2.internal/api/webhooks/slack', {
    body,
    headers,
    method: 'POST'
  })
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`timed out after ${timeoutMs}ms`)), timeoutMs)
      })
    ])
  } finally {
    clearTimeout(timer)
  }
}

async function sleep(ms: number): Promise<void> {
  await new Promise(resolve => setTimeout(resolve, ms))
}
