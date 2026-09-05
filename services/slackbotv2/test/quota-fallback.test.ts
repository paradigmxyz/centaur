import { describe, expect, test } from 'bun:test'
import { isProviderQuotaAnswer, quotaFallbackDecision } from '../src/quota-fallback'

const baseInput = {
  alreadyAttempted: false,
  configuredFallbackHarness: 'claudecode',
  defaultHarness: 'codex',
  explicitHarnessRequested: false,
  failedHarness: 'codex'
}

describe('quotaFallbackDecision', () => {
  test('moves the default harness to the configured fallback', () => {
    expect(quotaFallbackDecision(baseInput)).toEqual({
      harnessType: 'claudecode',
      outcome: 'scheduled'
    })
  })

  test('moves a depleted fallback harness back to the deployment default', () => {
    expect(
      quotaFallbackDecision({
        ...baseInput,
        failedHarness: 'claudecode'
      })
    ).toEqual({ harnessType: 'codex', outcome: 'scheduled' })
  })

  test('does not override an explicit harness request on the failed message', () => {
    expect(
      quotaFallbackDecision({
        ...baseInput,
        explicitHarnessRequested: true
      })
    ).toEqual({ outcome: 'suppressed_explicit_harness' })
  })

  test('does not attempt a second switch for the same message', () => {
    expect(
      quotaFallbackDecision({
        ...baseInput,
        alreadyAttempted: true
      })
    ).toEqual({ outcome: 'suppressed_already_attempted' })
  })

  test('rejects unsupported configured and default harnesses', () => {
    expect(
      quotaFallbackDecision({
        ...baseInput,
        configuredFallbackHarness: 'unknown'
      })
    ).toEqual({ harnessType: 'unknown', outcome: 'misconfigured' })
    expect(
      quotaFallbackDecision({
        ...baseInput,
        defaultHarness: 'unknown',
        failedHarness: 'claudecode'
      })
    ).toEqual({ harnessType: 'unknown', outcome: 'misconfigured' })
  })
})

describe('isProviderQuotaAnswer', () => {
  test('matches the exact Claude session-limit banner', () => {
    expect(isProviderQuotaAnswer("You've hit your session limit · resets 8:20pm (UTC)")).toBe(true)
    expect(isProviderQuotaAnswer("  You’ve hit your session limit\n")).toBe(true)
  })

  test('does not mistake explanatory prose for a provider quota banner', () => {
    expect(isProviderQuotaAnswer('The logs say you have hit your session limit.')).toBe(false)
    expect(
      isProviderQuotaAnswer("You've hit your session limit, so here are your options.")
    ).toBe(false)
  })
})
