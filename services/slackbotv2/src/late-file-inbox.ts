import type { StateAdapter } from 'chat'
import type { SlackbotV2ApiMessage, SlackbotV2ExecuteSessionRequest } from './types'
import { withStateLease } from './state-lease'

const JOB_TTL_MS = 24 * 60 * 60_000
// Slack can redeliver for a full day. Retention is independent of the 15s
// message-to-file association window, which every candidate must still satisfy.
export const LATE_FILE_PENDING_TTL_MS = JOB_TTL_MS + 60_000

export type LateFileMention = {
  channel: string
  mentionTs: string
  teamId: string
  threadId: string
  threadTs: string
  user: string
}

export type LateFileJob = {
  event: Record<string, unknown>
  mention: LateFileMention
  expiresAt: number
  executeAttempted: boolean
  preparedExecute?: {
    message: SlackbotV2ApiMessage
    request: SlackbotV2ExecuteSessionRequest
  }
  afterEventId?: number
  complete: boolean
  attempts: number
  nextAttemptAt: number
}

export type LateFileHandoff = {
  resumeExecute: boolean
  afterEventId?: number
  preparedExecute?: LateFileJob['preparedExecute']
  beforeExecute(afterEventId: number, preparedExecute?: LateFileJob['preparedExecute']): Promise<void>
  /** Whether this worker still holds the job. A stale worker must not write thread state. */
  owned(): boolean
  /** Renews the job lease, throwing once a successor may own it. */
  assertOwned(): Promise<void>
}

export class LateFilePermanentError extends Error {}

// Each job keeps the original Slack message identity, including on an uncertain
// API response. The session API deduplicates append and execute by that identity.
export function createLateFileInbox(input: {
  state: StateAdapter
  repair(job: LateFileJob, handoff: LateFileHandoff): Promise<void>
  failed(error: unknown): void
  retryDelay(attempt: number): number
  isPermanentFailure?(error: unknown): boolean
  abandoned?(job: LateFileJob): Promise<void>
}) {
  const { state } = input
  const jobKey = (eventKey: string) => `slackbotv2:late-file:job:${eventKey}`
  const pendingKey = (team: string, channel: string, user: string, time: number) =>
    `slackbotv2:late-file:pending:${JSON.stringify([team, channel, user, Math.floor(time / 60_000)])}`
  const claimKey = (mention: LateFileMention) =>
    `slackbotv2:late-file:mention:${JSON.stringify([
      mention.teamId, mention.channel, mention.user, mention.mentionTs
    ])}`
  const indexKey = (time: number) =>
    `slackbotv2:late-file:inbox:${new Date(time).toISOString().slice(0, 10)}`
  const remaining = (job: LateFileJob) => Math.max(1, job.expiresAt - Date.now())

  async function run(key: string): Promise<number | null> {
    const existing = await state.get<LateFileJob>(key)
    if (!existing || existing.complete || existing.expiresAt <= Date.now()) return null
    const result = await withStateLease(state, `${key}:processing`, async lease => {
      const job = await state.get<LateFileJob>(key)
      if (!job || job.complete || job.expiresAt <= Date.now()) return null
      if (job.nextAttemptAt > Date.now()) return job.nextAttemptAt - Date.now()

      // Ownership is not a processing lease: a different event must never take
      // this mention, even after the worker crashes or the lease expires.
      const claimTtl = Math.max(job.expiresAt,
        Number(job.mention.mentionTs) * 1000 + LATE_FILE_PENDING_TTL_MS) - Date.now()
      await state.setIfNotExists(claimKey(job.mention), key, Math.max(1, claimTtl))
      if (await state.get<string>(claimKey(job.mention)) !== key) {
        await state.set(key, { ...job, complete: true }, remaining(job))
        return null
      }
      const resumesExecute = job.executeAttempted
      try {
        await lease.assert()
        await input.repair(job, {
          resumeExecute: job.executeAttempted,
          afterEventId: job.afterEventId,
          preparedExecute: job.preparedExecute,
          owned: lease.valid,
          assertOwned: lease.assert,
          beforeExecute: async (afterEventId, preparedExecute) => {
            await lease.assert()
            const preparedJob = {
              ...job,
              executeAttempted: true,
              afterEventId: job.afterEventId ?? afterEventId,
              preparedExecute: job.preparedExecute ?? preparedExecute
            }
            await state.set(key, preparedJob, remaining(job))
            Object.assign(job, preparedJob)
          }
        })
        await lease.assert()
        // Render recovery now owns delivery; retain only the dedupe tombstone.
        await state.set(key, { ...job, preparedExecute: undefined, complete: true }, remaining(job))
        return null
      } catch (error) {
        input.failed(error)
        // A worker that lost its lease cannot overwrite a successor's result.
        if (!lease.valid()) return 1_000
        await lease.assert()
        // An earlier uncertain execute may already have been accepted. Changed
        // file/API permissions must not abandon its still-undelivered answer.
        if (
          !resumesExecute
          && (error instanceof LateFilePermanentError || input.isPermanentFailure?.(error))
        ) {
          await abandon(job)
          return null
        }
        const delay = input.retryDelay(job.attempts++)
        job.nextAttemptAt = Date.now() + delay
        await state.set(key, job, remaining(job))
        return delay
      }

      async function abandon(job: LateFileJob): Promise<void> {
        await lease.assert()
        // Keep the completed tombstone and mention claim. Redelivery must not
        // retry either the session handoff or this best-effort error notice.
        await state.set(key, { ...job, complete: true }, remaining(job))
        try {
          await input.abandoned?.(job)
        } catch (error) {
          input.failed(error)
        }
      }
    })
    return result ? result.value : 1_000
  }

  return {
    async remember(mention: LateFileMention): Promise<void> {
      const ttlMs = Number(mention.mentionTs) * 1000 + LATE_FILE_PENDING_TTL_MS - Date.now()
      if (!Number.isFinite(ttlMs) || ttlMs <= 0) return
      await state.appendToList(pendingKey(
        mention.teamId, mention.channel, mention.user, Number(mention.mentionTs) * 1000
      ), mention, {
        ttlMs
      })
    },

    async accept(eventKey: string, event: Record<string, unknown>, identity: {
      teamId: string
      channel: string
      user: string
      ts: string
      threadTs: string
    }): Promise<string | null> {
      const key = jobKey(eventKey)
      // A retry uses its durable original, even after the matching window closes.
      if (await state.get<LateFileJob>(key)) return key
      const eventMs = Number(identity.ts) * 1000
      if (!Number.isFinite(eventMs)) return null
      // Only these two minute buckets can contain a mention within 15s of
      // this file, even when its authenticated delivery is a day late.
      const keys = Array.from(new Set([eventMs, eventMs - 15_000].map(time =>
        pendingKey(identity.teamId, identity.channel, identity.user, time)
      )))
      const entries = (await Promise.all(keys.map(key => state.getList<LateFileMention>(key)))).flat()
      const mention = entries.sort((a, b) => Number(b.mentionTs) - Number(a.mentionTs)).find(entry => {
        const mentionMs = Number(entry.mentionTs) * 1000
        return (!identity.threadTs || identity.threadTs === entry.threadTs)
          && eventMs > mentionMs && eventMs - mentionMs <= 15_000
          && mentionMs >= Date.now() - LATE_FILE_PENDING_TTL_MS
      })
      if (!mention) return null

      // Index first: every acknowledged job is discoverable after a crash.
      // Daily buckets avoid refreshing older days' list TTLs on every append.
      await state.appendToList(indexKey(Date.now()), key, { ttlMs: 2 * JOB_TTL_MS })
      await state.setIfNotExists(key, {
        event,
        mention,
        expiresAt: Date.now() + JOB_TTL_MS,
        executeAttempted: false,
        complete: false,
        attempts: 0,
        nextAttemptAt: 0
      } satisfies LateFileJob, JOB_TTL_MS)
      return key
    },

    run,

    async recover(): Promise<number | null> {
      const lists = await Promise.all([
        state.getList<string>(indexKey(Date.now())),
        state.getList<string>(indexKey(Date.now() - JOB_TTL_MS))
      ])
      const keys = Array.from(new Set(lists.flat()))
      let next: number | null = null
      // A busy thread must not hold up unrelated threads in the restart backlog.
      let cursor = 0
      await Promise.all(Array.from({ length: Math.min(4, keys.length) }, async () => {
        while (cursor < keys.length) {
          const key = keys[cursor++]!
          let delay: number | null
          try {
            delay = await run(key)
          } catch (error) {
            input.failed(error)
            delay = 5_000
          }
          if (delay !== null) next = next === null ? delay : Math.min(next, delay)
        }
      }))
      return next
    }
  }
}
