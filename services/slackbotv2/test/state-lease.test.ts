import { expect, it, spyOn } from 'bun:test'
import { createMemoryState } from '@chat-adapter/state-memory'
import { withStateLease } from '../src/state-lease'

async function connectedMemory() {
  const state = createMemoryState()
  await state.connect()
  return state
}

it('retries a renewal transport failure while the confirmed lease is valid', async () => {
  const state = await connectedMemory()
  const renew = spyOn(state, 'extendLock').mockRejectedValueOnce(new Error('database blip'))
  try {
    const result = await withStateLease(state, 'handoff', async lease => {
      await Promise.all([lease.assert(), lease.assert()])
      expect(lease.valid()).toBe(true)
      expect(renew).toHaveBeenCalledTimes(2)
      return 'kept ownership'
    }, 300)
    expect(result?.value).toBe('kept ownership')
  } finally {
    renew.mockRestore()
  }
})

it('keeps renewing a live handoff past its original TTL after a heartbeat error', async () => {
  const state = await connectedMemory()
  const renew = spyOn(state, 'extendLock').mockRejectedValueOnce(new Error('database blip'))
  try {
    await withStateLease(state, 'handoff', async lease => {
      await Bun.sleep(220)
      expect(renew.mock.calls.length).toBeGreaterThan(2)
      expect(lease.valid()).toBe(true)
      expect(await withStateLease(state, 'handoff', async () => 'second owner', 150)).toBeNull()
      await lease.assert()
    }, 150)
    expect((await withStateLease(state, 'handoff', async () => 'successor', 150))?.value).toBe('successor')
  } finally {
    renew.mockRestore()
  }
})

it('loses ownership on an explicit rejection even before the deadline', async () => {
  const state = await connectedMemory()
  const renew = spyOn(state, 'extendLock').mockResolvedValueOnce(false)
  try {
    await withStateLease(state, 'handoff', async lease => {
      await expect(lease.assert()).rejects.toThrow('Shared state lease lost')
      expect(lease.valid()).toBe(false)
    })
  } finally {
    renew.mockRestore()
  }
})

it('never extends uncertain ownership past the last confirmed deadline', async () => {
  const state = await connectedMemory()
  const now = Date.now()
  const clock = spyOn(Date, 'now').mockReturnValue(now)
  const renew = spyOn(state, 'extendLock').mockImplementation(async () => {
    clock.mockReturnValue(now + 300)
    throw new Error('database unavailable')
  })
  try {
    await withStateLease(state, 'handoff', async lease => {
      await expect(lease.assert()).rejects.toThrow('Shared state lease lost')
      expect(lease.valid()).toBe(false)
    }, 300)
  } finally {
    clock.mockRestore()
    renew.mockRestore()
  }
})

it('does not revive a successful renewal that returns at the old deadline', async () => {
  const state = await connectedMemory()
  const now = Date.now()
  const clock = spyOn(Date, 'now').mockReturnValue(now)
  const renew = spyOn(state, 'extendLock').mockImplementation(async () => {
    clock.mockReturnValue(now + 300)
    return true
  })
  try {
    await withStateLease(state, 'handoff', async lease => {
      await expect(lease.assert()).rejects.toThrow('Shared state lease lost')
      expect(lease.valid()).toBe(false)
    }, 300)
  } finally {
    clock.mockRestore()
    renew.mockRestore()
  }
})
