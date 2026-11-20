import { describe, expect, it, spyOn } from 'bun:test'
import { createMemoryState } from '@chat-adapter/state-memory'
import {
  createLateFileInbox, LateFilePermanentError,
  type LateFileJob, type LateFileMention
} from '../src/late-file-inbox'

async function connectedMemory() {
  const state = createMemoryState()
  await state.connect()
  return state
}

function mention(): LateFileMention {
  const ts = ((Date.now() - 20_000) / 1000).toFixed(6)
  return { channel: 'C1', teamId: 'T1', user: 'U1', mentionTs: ts, threadTs: ts, threadId: `slack:C1:${ts}` }
}

function identity(pending: LateFileMention, overrides = {}) {
  return { ...pending, ts: (Number(pending.mentionTs) + 2).toFixed(6), threadTs: '', ...overrides }
}

describe('durable late-file inbox', () => {
  it('completes permanent failures before notifying and keeps the tombstone on redelivery', async () => {
    const state = await connectedMemory()
    let repairs = 0
    let notices = 0
    let key: string
    const worker = () => createLateFileInbox({
      state, failed: () => {}, retryDelay: () => 0,
      repair: async () => { repairs++; throw new LateFilePermanentError('file deleted') },
      abandoned: async () => {
        expect((await state.get<LateFileJob>(key))?.complete).toBe(true)
        notices++
        throw new Error('notice failed')
      }
    })
    const pending = mention()
    await worker().remember(pending)
    key = (await worker().accept('permanent', {}, identity(pending)))!
    expect(await worker().run(key)).toBeNull()
    await worker().accept('permanent', {}, identity(pending))
    await worker().recover()
    await worker().run((await worker().accept('other-delivery', {}, identity(pending)))!)
    expect(repairs).toBe(1)
    expect(notices).toBe(1)
  })

  it('continues uncertain execution recovery across repeated transient errors and restart', async () => {
    const state = await connectedMemory()
    let repairs = 0
    const worker = () => createLateFileInbox({
      state, failed: () => {}, retryDelay: () => 0,
      repair: async (_, handoff) => {
        repairs++
        expect(handoff.resumeExecute).toBe(repairs > 1)
        await handoff.beforeExecute(10)
        if (repairs <= 10) throw new Error('execute response lost')
      }
    })
    const pending = mention()
    await worker().remember(pending)
    const key = (await worker().accept('uncertain-many', {}, identity(pending)))!
    for (let attempt = 0; attempt < 12; attempt++) await worker().run(key)
    expect(repairs).toBe(11)
    expect((await state.get<LateFileJob>(key))?.complete).toBe(true)
  })

  it('does not abandon an uncertain earlier execute when file access changes', async () => {
    const state = await connectedMemory()
    let repairs = 0
    let notices = 0
    const worker = () => createLateFileInbox({
      state, failed: () => {}, retryDelay: () => 0,
      repair: async (_, handoff) => {
        repairs++
        if (repairs === 1) {
          await handoff.beforeExecute(10)
          throw new Error('execute response lost')
        }
        expect(handoff.resumeExecute).toBe(true)
        if (repairs === 2) throw new LateFilePermanentError('file access denied')
      },
      abandoned: async () => { notices++ }
    })
    const pending = mention()
    await worker().remember(pending)
    const key = (await worker().accept('uncertain-permanent', {}, identity(pending)))!
    await worker().run(key)
    await worker().run(key)
    expect((await state.get<LateFileJob>(key))?.complete).toBe(false)
    await worker().recover()
    expect((await state.get<LateFileJob>(key))?.complete).toBe(true)
    expect(repairs).toBe(3)
    expect(notices).toBe(0)
  })

  it('shares pending mentions and one-time ownership across independent workers and restart', async () => {
    const state = await connectedMemory()
    let repairs = 0
    const worker = () => createLateFileInbox({
      state,
      repair: async () => { repairs++; await Bun.sleep(10) },
      failed: error => { throw error }, retryDelay: () => 0
    })
    const first = worker()
    const pending = mention()
    await first.remember(pending)
    const second = worker()
    const key = await second.accept('event-1', { ts: 'file-1' }, identity(pending))
    expect(key).not.toBeNull()
    await Promise.all([first.run(key!), second.run(key!)])
    await worker().recover()
    expect(repairs).toBe(1)
    const other = await worker().accept('event-2', { ts: 'file-2' }, identity(pending))
    await worker().run(other!)
    expect(repairs).toBe(1)
  })

  it('lets only one of two distinct concurrent events consume a mention', async () => {
    const state = await connectedMemory()
    let repairs = 0
    const inbox = createLateFileInbox({
      state, repair: async () => { repairs++; await Bun.sleep(10) },
      failed: error => { throw error }, retryDelay: () => 0
    })
    const pending = mention()
    await inbox.remember(pending)
    const keys = await Promise.all([
      inbox.accept('event-a', {}, identity(pending)),
      inbox.accept('event-b', {}, identity(pending))
    ])
    await Promise.all(keys.map(key => inbox.run(key!)))
    expect(repairs).toBe(1)
  })

  it('recovers an uncertain execute after failure without requiring another Slack delivery', async () => {
    const state = await connectedMemory()
    const attempts: boolean[] = []
    const worker = () => createLateFileInbox({
      state,
      repair: async (_, handoff) => {
        attempts.push(handoff.resumeExecute)
        await handoff.beforeExecute(0)
        if (attempts.length === 1) throw new Error('worker lost the execute response')
      },
      failed: () => {}, retryDelay: () => 0
    })
    const pending = mention()
    await worker().remember(pending)
    const key = await worker().accept('uncertain', {}, identity(pending))
    await worker().run(key!)
    await worker().recover()
    await worker().run(key!)
    expect(attempts).toEqual([false, true])
  })

  it('recovers a job after a crashed worker processing lease expires', async () => {
    const state = await connectedMemory()
    let repairs = 0
    const worker = () => createLateFileInbox({
      state, repair: async () => { repairs++ }, failed: () => {}, retryDelay: () => 0
    })
    const pending = mention()
    await worker().remember(pending)
    const key = await worker().accept('crashed', {}, identity(pending))
    await state.acquireLock(`${key}:processing`, 10)
    expect(await worker().run(key!)).not.toBeNull()
    expect(repairs).toBe(0)
    await Bun.sleep(20)
    await worker().recover()
    expect(repairs).toBe(1)
  })

  it('does not lose a failed job write when Slack retries after the old one-minute TTL', async () => {
    const state = await connectedMemory()
    let repairs = 0
    const inbox = createLateFileInbox({
      state, repair: async () => { repairs++ }, failed: () => {}, retryDelay: () => 0
    })
    const pending = mention()
    await inbox.remember(pending)
    const write = spyOn(state, 'setIfNotExists').mockRejectedValueOnce(new Error('database unavailable'))
    await expect(inbox.accept('retry-write', {}, identity(pending))).rejects.toThrow('database unavailable')
    write.mockRestore()
    await inbox.recover()
    expect(repairs).toBe(0)
    const now = Date.now()
    const clock = spyOn(Date, 'now').mockReturnValue(now + 61_000)
    try {
      await inbox.accept('retry-write', {}, identity(pending))
      await inbox.recover()
    } finally {
      clock.mockRestore()
    }
    expect(repairs).toBe(1)
  })

  it('does not let a suspended worker undo its successor completion', async () => {
    const state = await connectedMemory()
    let release!: () => void
    let entered!: () => void
    const started = new Promise<void>(resolve => { entered = resolve })
    const suspended = new Promise<void>(resolve => { release = resolve })
    let calls = 0
    const inbox = createLateFileInbox({
      state, failed: () => {}, retryDelay: () => 0,
      repair: async () => {
        if (++calls === 1) {
          entered()
          await suspended
          throw new Error('suspended old worker failed')
        }
      }
    })
    const pending = mention()
    await inbox.remember(pending)
    const key = (await inbox.accept('suspended', {}, identity(pending)))!
    const old = inbox.run(key)
    await started
    const now = Date.now()
    const clock = spyOn(Date, 'now').mockReturnValue(now + 121_000)
    try {
      await inbox.run(key)
      release()
      await old
      expect((await state.get<{ complete: boolean }>(key))?.complete).toBe(true)
    } finally {
      release()
      clock.mockRestore()
    }
  })

  it('does not revive ownership when a successful renewal response arrives after expiry', async () => {
    const state = await connectedMemory()
    let release!: () => void
    let entered!: () => void
    const started = new Promise<void>(resolve => { entered = resolve })
    const delayed = new Promise<void>(resolve => { release = resolve })
    const extend = state.extendLock.bind(state)
    const renewal = spyOn(state, 'extendLock').mockImplementationOnce(async (lock, ttl) => {
      const result = await extend(lock, ttl)
      entered()
      await delayed
      return result
    })
    let repairs = 0
    const inbox = createLateFileInbox({
      state, failed: () => {}, retryDelay: () => 0,
      repair: async () => {
        if (++repairs > 1) throw new Error('expired worker resumed')
      }
    })
    const pending = mention()
    await inbox.remember(pending)
    const key = (await inbox.accept('delayed-renewal', {}, identity(pending)))!
    const old = inbox.run(key)
    await started
    const now = Date.now()
    const clock = spyOn(Date, 'now').mockReturnValue(now + 121_000)
    try {
      await inbox.run(key)
      release()
      await old
      expect(repairs).toBe(1)
      expect((await state.get<{ complete: boolean }>(key))?.complete).toBe(true)
    } finally {
      release()
      clock.mockRestore()
      renewal.mockRestore()
    }
  })

  it('keeps mention ownership through the end of pending eligibility', async () => {
    const state = await connectedMemory()
    let repairs = 0
    const inbox = createLateFileInbox({
      state, repair: async () => { repairs++ }, failed: () => {}, retryDelay: () => 0
    })
    const pending = mention()
    await inbox.remember(pending)
    const first = (await inbox.accept('first-day', {}, identity(pending)))!
    await inbox.run(first)
    const now = Date.now()
    const clock = spyOn(Date, 'now').mockReturnValue(now + 24 * 60 * 60_000 + 1)
    try {
      const second = (await inbox.accept('next-day', {}, identity(pending)))!
      expect(second).not.toBeNull()
      await inbox.run(second)
      expect(repairs).toBe(1)
    } finally {
      clock.mockRestore()
    }
  })

  it('keeps four workers busy while one unrelated thread is waiting', async () => {
    const state = await connectedMemory()
    let release!: () => void
    const blocked = new Promise<void>(resolve => { release = resolve })
    let reachedFifth!: () => void
    const fifth = new Promise<void>(resolve => { reachedFifth = resolve })
    let active = 0
    let peak = 0
    const inbox = createLateFileInbox({
      state, failed: () => {}, retryDelay: () => 0,
      repair: async job => {
        active++
        peak = Math.max(peak, active)
        if (job.event.index === 0) await blocked
        if (job.event.index === 4) reachedFifth()
        await Bun.sleep(1)
        active--
      }
    })
    for (let index = 0; index < 5; index++) {
      const pending = { ...mention(), user: `U${index}` }
      await inbox.remember(pending)
      await inbox.accept(`backlog-${index}`, { index }, identity(pending))
    }
    const recovery = inbox.recover()
    try {
      await Promise.race([fifth, Bun.sleep(100).then(() => { throw new Error('idle slots blocked') })])
      expect(peak).toBeLessThanOrEqual(4)
    } finally {
      release()
      await recovery
    }
  })

  it.each([
    { teamId: 'T2' }, { channel: 'C2' }, { user: 'U2' }, { threadTs: 'wrong-thread' }
  ])('rejects an identity mismatch: %j', async overrides => {
    const inbox = createLateFileInbox({
      state: await connectedMemory(), repair: async () => {}, failed: () => {}, retryDelay: () => 0
    })
    const pending = mention()
    await inbox.remember(pending)
    expect(await inbox.accept('mismatch', {}, identity(pending, overrides))).toBeNull()
  })

  it.each([[0, false], [15, true], [15.001, false]] as const)(
    'enforces the mention-to-file window at %s seconds', async (seconds, accepted) => {
      const inbox = createLateFileInbox({
        state: await connectedMemory(), repair: async () => {}, failed: () => {}, retryDelay: () => 0
      })
      const pending = mention()
      await inbox.remember(pending)
      const key = await inbox.accept('boundary', {}, identity(pending, {
        ts: (Number(pending.mentionTs) + seconds).toFixed(6)
      }))
      expect(key !== null).toBe(accepted)
    }
  )

  it('rejects a mention beyond the retry horizon even when another row refreshes its bucket TTL', async () => {
    const state = await connectedMemory()
    const inbox = createLateFileInbox({
      state, repair: async () => {}, failed: () => {}, retryDelay: () => 0
    })
    const old = { ...mention(), mentionTs: ((Date.now() - 25 * 60 * 60_000) / 1000).toFixed(6) }
    await state.appendToList(`slackbotv2:late-file:pending:${JSON.stringify([
      old.teamId, old.channel, old.user, Math.floor(Number(old.mentionTs) * 1000 / 60_000)
    ])}`, old, { ttlMs: 60_000 })
    expect(await inbox.accept('expired', {}, identity(old))).toBeNull()
  })
})
