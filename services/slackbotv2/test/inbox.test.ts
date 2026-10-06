import { createHmac, randomUUID } from 'node:crypto'
import { afterEach, describe, expect, it } from 'bun:test'
import { verifySlackSignature } from '@chat-adapter/slack/webhook'
import pg from 'pg'
import { createSlackInbox, currentSlackInboxTaskId, type SlackInbox } from '../src/inbox'
import { noopLogger } from '../src/utils'

// A database initialized with the Absurd schema (api-rs migrations 0007-0009).
const postgresUrl = process.env.SLACKBOTV2_TEST_DATABASE_URL
const signingSecret = 'inbox-signing-secret'
const cleanups: Array<() => Promise<void>> = []

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
})

function signedHeaders(body: string): Headers {
  const timestamp = Math.floor(Date.now() / 1000).toString()
  const signature = createHmac('sha256', signingSecret)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')
  return new Headers({
    'x-slack-request-timestamp': timestamp,
    'x-slack-signature': `v0=${signature}`
  })
}

function startInbox(
  queue: string,
  deliver: (request: Request) => Promise<void>
): { inbox: SlackInbox; pool: pg.Pool } {
  const pool = new pg.Pool({ connectionString: postgresUrl })
  const inbox = createSlackInbox({
    concurrency: 4,
    deliver: async request => {
      await deliver(request)
      return new Response('ok')
    },
    leaseSeconds: 1,
    logger: noopLogger,
    maxAgeSeconds: 60,
    pool,
    queue,
    signingSecret
  })
  cleanups.push(async () => {
    await inbox.close()
    await pool.end().catch(() => undefined)
  })
  return { inbox, pool }
}

async function waitFor(predicate: () => boolean, timeoutMs = 5_000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('timed out waiting for condition')
    await Bun.sleep(20)
  }
}

describe.skipIf(!postgresUrl)('Slack inbox', () => {
  it('delivers a queued request once, freshly signed, and absorbs Slack retries', async () => {
    const delivered: Request[] = []
    const { inbox } = startInbox(`test_${randomUUID().replaceAll('-', '')}`, async request => {
      delivered.push(request)
    })
    const body = JSON.stringify({ event_id: 'Ev-inbox-once', type: 'event_callback' })

    expect((await inbox.accept(body, signedHeaders(body)))?.status).toBe(200)
    await waitFor(() => delivered.length === 1)
    const retry = signedHeaders(body)
    retry.set('x-slack-retry-num', '1')
    expect((await inbox.accept(body, retry))?.status).toBe(200)

    await Bun.sleep(500)
    expect(delivered).toHaveLength(1)
    const request = delivered[0]!
    const text = await request.text()
    expect(text).toBe(body)
    await verifySlackSignature(text, request.headers, { signingSecret })
  })

  it('rejects unsigned requests', async () => {
    const { inbox } = startInbox(`test_${randomUUID().replaceAll('-', '')}`, async () => {})
    expect((await inbox.accept('{}', new Headers()))?.status).toBe(401)
  })

  it('redelivers a request whose process died mid-delivery once its lease expires', async () => {
    const queue = `test_${randomUUID().replaceAll('-', '')}`
    const firstTaskIds: string[] = []
    let releaseFirst = () => {}
    const first = startInbox(queue, async () => {
      firstTaskIds.push(currentSlackInboxTaskId()!)
      await new Promise<void>(resolve => {
        releaseFirst = resolve
      })
    })
    const body = JSON.stringify({ event_id: 'Ev-inbox-crash', type: 'event_callback' })
    expect((await first.inbox.accept(body, signedHeaders(body)))?.status).toBe(200)
    await waitFor(() => firstTaskIds.length === 1)

    // The first process loses its database, so its lease is never extended.
    await first.pool.end()
    const redelivered: Array<{ body: string; taskId?: string }> = []
    startInbox(queue, async request => {
      redelivered.push({ body: await request.text(), taskId: currentSlackInboxTaskId() })
    })

    await waitFor(() => redelivered.length === 1)
    expect(redelivered).toEqual([{ body, taskId: firstTaskIds[0] }])
    releaseFirst()
  })
})
