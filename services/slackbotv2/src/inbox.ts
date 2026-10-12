import { AsyncLocalStorage } from 'node:async_hooks'
import { createHmac } from 'node:crypto'
import { verifySlackSignature } from '@chat-adapter/slack/webhook'
import { Absurd } from 'absurd-sdk'
import type { Logger, WebhookOptions } from 'chat'
import type pg from 'pg'
import { errorMessage } from './utils'

const TASK_NAME = 'slack_webhook'
// Idempotency keys must outlive Slack's retries (minutes); bodies need not.
const CLEANUP_TTL = '1 hour'
const CLEANUP_INTERVAL_MS = 10 * 60 * 1000
const START_RETRY_MAX_DELAY_MS = 10_000
// Leave time to handle a request synchronously while the queue cannot be created.
const QUEUE_READY_TIMEOUT_MS = 500

const currentTask = new AsyncLocalStorage<string>()

/** The inbox task delivering the current Slack request, if any. */
export function currentSlackInboxTaskId(): string | undefined {
  return currentTask.getStore()
}

/** Absurd queue for a deployment; `namespace` keeps deployments that share a database apart. */
export function slackInboxQueueName(namespace: string): string {
  return `${namespace}_inbox`.toLowerCase().replace(/[^a-z0-9_]/g, '_').slice(0, 57)
}

export type SlackInbox = ReturnType<typeof createSlackInbox>

/**
 * Holds verified Slack webhook requests in an Absurd queue so Slack can be
 * acknowledged before the Chat SDK handlers run. A worker feeds each request
 * to the Chat SDK once; if its process dies first, the task's lease expires
 * and any replica feeds it again. Slack retries of a queued event are
 * acknowledged without queueing it twice.
 */
export function createSlackInbox(deps: {
  /** Feeds a request to the Chat SDK, e.g. `chat.webhooks.slack`. */
  deliver: (request: Request, options: WebhookOptions) => Promise<Response>
  logger: Logger
  /** Seconds a crashed delivery holds its task; must exceed the Chat SDK duplicate window. */
  leaseSeconds: number
  /** Seconds after which a request is too old to answer. */
  maxAgeSeconds: number
  pool: pg.Pool
  queue: string
  signingSecret: string
  concurrency: number
}) {
  const { logger, queue } = deps
  const absurd = new Absurd({
    db: deps.pool,
    queueName: queue,
    log: {
      log: (...args) => logger.debug('slackbotv2_inbox_absurd', { message: args.join(' ') }),
      info: (...args) => logger.info('slackbotv2_inbox_absurd', { message: args.join(' ') }),
      warn: (...args) => logger.warn('slackbotv2_inbox_absurd', { message: args.join(' ') }),
      error: (...args) => logger.error('slackbotv2_inbox_absurd', { message: args.join(' ') })
    }
  })

  absurd.registerTask<{ body: string }>(
    {
      name: TASK_NAME,
      // Only a lost lease retries; a failed delivery is not fed again.
      defaultMaxAttempts: 3,
      defaultCancellation: { maxDelay: deps.maxAgeSeconds, maxDuration: deps.maxAgeSeconds }
    },
    async ({ body }, ctx) => {
      const heartbeat = setInterval(
        () => void ctx.heartbeat().catch(() => undefined),
        (deps.leaseSeconds * 1000) / 3
      )
      try {
        await currentTask.run(ctx.taskID, async () => {
          const tasks: Promise<unknown>[] = []
          const response = await deps.deliver(signedSlackRequest(body, deps.signingSecret), {
            waitUntil: task => {
              tasks.push(task)
            }
          })
          if (!response.ok) {
            throw new Error(`Chat SDK rejected the request with HTTP ${response.status}`)
          }
          await Promise.all(tasks)
        })
      } catch (error) {
        logger.warn('slackbotv2_inbox_delivery_failed', {
          error: errorMessage(error),
          inbox_task_id: ctx.taskID
        })
      } finally {
        clearInterval(heartbeat)
      }
    }
  )

  let closed = false
  let worker: { close(): Promise<void> } | undefined
  let queueCreated = () => {}
  const queueReady = new Promise<void>(resolve => {
    queueCreated = resolve
  })
  const cleanup = setInterval(() => {
    deps.pool.query('SELECT * FROM absurd.cleanup_all_queues($1)', [queue]).catch(error => {
      logger.warn('slackbotv2_inbox_cleanup_failed', { error: errorMessage(error) })
    })
  }, CLEANUP_INTERVAL_MS)
  cleanup.unref?.()

  const started = (async () => {
    for (let attempt = 1; !closed; attempt += 1) {
      try {
        await absurd.createQueue(queue, { cleanupTtl: CLEANUP_TTL })
        queueCreated()
        if (closed) return
        worker = await absurd.startWorker({
          claimTimeout: deps.leaseSeconds,
          concurrency: deps.concurrency,
          // A handler past its lease is replayed elsewhere; never kill the process for it.
          fatalOnLeaseTimeout: false,
          onError: error => {
            logger.warn('slackbotv2_inbox_worker_error', { error: errorMessage(error) })
          }
        })
        logger.info('slackbotv2_inbox_started', { queue })
        return
      } catch (error) {
        logger.warn('slackbotv2_inbox_start_failed', { attempt, error: errorMessage(error) })
        await sleep(Math.min(250 * 2 ** attempt, START_RETRY_MAX_DELAY_MS))
      }
    }
  })()

  return {
    /**
     * Verifies and queues a Slack webhook request, returning the response for
     * Slack, or null when the queue is not ready and the caller must feed the
     * request to the Chat SDK synchronously.
     */
    async accept(body: string, headers: Headers): Promise<Response | null> {
      try {
        await verifySlackSignature(body, headers, { signingSecret: deps.signingSecret })
      } catch (error) {
        logger.warn('slackbotv2_inbox_signature_rejected', { error: errorMessage(error) })
        return new Response('Invalid signature', { status: 401 })
      }
      try {
        await withTimeout(queueReady, QUEUE_READY_TIMEOUT_MS)
      } catch {
        logger.warn('slackbotv2_inbox_not_ready', { queue })
        return null
      }
      const eventId = slackEventId(body)
      try {
        const spawned = await absurd.spawn(
          TASK_NAME,
          { body },
          eventId ? { idempotencyKey: `slack-event:${eventId}` } : {}
        )
        logger.info('slackbotv2_inbox_queued', {
          duplicate: !spawned.created,
          inbox_task_id: spawned.taskID,
          slack_event_id: eventId
        })
      } catch (error) {
        // Slack retries the event; its idempotency key absorbs a late spawn.
        logger.warn('slackbotv2_inbox_queue_failed', { error: errorMessage(error) })
        return new Response('Slack inbox unavailable', { status: 503 })
      }
      return new Response('ok', { status: 200 })
    },

    /** Whether the inbox task may still deliver, i.e. has not finished, failed, or been dropped. */
    async isTaskLive(taskId: string): Promise<boolean> {
      try {
        const result = await absurd.fetchTaskResult(taskId)
        return result !== null && ['pending', 'running', 'sleeping'].includes(result.state)
      } catch (error) {
        logger.warn('slackbotv2_inbox_task_lookup_failed', {
          error: errorMessage(error),
          inbox_task_id: taskId
        })
        return true
      }
    },

    /** Stops taking tasks and waits for running deliveries to finish. */
    async close(): Promise<void> {
      closed = true
      clearInterval(cleanup)
      await started
      await worker?.close()
    }
  }
}

function slackEventId(body: string): string | undefined {
  try {
    const eventId = (JSON.parse(body) as { event_id?: unknown }).event_id
    return typeof eventId === 'string' && eventId ? eventId : undefined
  } catch {
    return undefined
  }
}

/** A freshly signed copy of a verified Slack webhook body. */
function signedSlackRequest(body: string, signingSecret: string): Request {
  const timestamp = Math.floor(Date.now() / 1000).toString()
  const signature = createHmac('sha256', signingSecret)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')
  return new Request('http://slackbotv2.internal/api/webhooks/slack', {
    body,
    headers: {
      'content-type': 'application/json',
      'x-slack-request-timestamp': timestamp,
      'x-slack-signature': `v0=${signature}`
    },
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
