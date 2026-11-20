import type { StateAdapter } from 'chat'

export type StateLease = {
  valid(): boolean
  assert(): Promise<void>
}

// Use a separate key from the SDK's force-releasable thread lock. Backend
// idempotency still handles the uncertain result of an in-flight API request.
export async function withStateLease<T>(
  state: StateAdapter,
  key: string,
  work: (lease: StateLease) => Promise<T>,
  ttlMs = 120_000
): Promise<{ value: T } | null> {
  const lock = await state.acquireLock(key, ttlMs)
  if (!lock) return null
  let valid = true
  let heldUntil = lock.expiresAt
  let renewing: Promise<void> | undefined
  const owns = () => valid && Date.now() < heldUntil
  const renew = async () => {
    while (owns()) {
      const requestedAt = Date.now()
      const previousDeadline = heldUntil
      let extended: boolean
      try {
        extended = await state.extendLock(lock, ttlMs)
      } catch {
        // A transport failure is uncertain. Retry only while the last confirmed
        // lease still belongs to us; an expired worker must never revive itself.
        if (!owns()) break
        await new Promise(resolve => setTimeout(resolve,
          Math.min(1_000, ttlMs / 30, heldUntil - Date.now())))
        continue
      }
      // An RPC can succeed at the store and return after the old deadline.
      // Measure the new deadline from request time, never response time.
      if (!extended || !valid || Date.now() >= previousDeadline || Date.now() >= requestedAt + ttlMs) break
      heldUntil = Math.max(heldUntil, requestedAt + ttlMs)
      return
    }
    valid = false
    throw new Error('Shared state lease lost')
  }
  const extend = () => {
    // Timer renewals and assertions share one RPC/retry chain.
    renewing ??= renew().finally(() => { renewing = undefined })
    return renewing
  }
  const timer = setInterval(() => {
    void extend().catch(() => {})
  }, ttlMs / 3)
  timer.unref()
  try {
    return { value: await work({ valid: owns, assert: extend }) }
  } finally {
    clearInterval(timer)
    valid = false
    await renewing?.catch(() => {})
    await state.releaseLock(lock)
  }
}
