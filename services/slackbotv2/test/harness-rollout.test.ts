import { describe, expect, test } from 'bun:test'
import { resolveHarnessRollout } from '../src/harness-rollout'

describe('resolveHarnessRollout', () => {
  test('assigns Codex requests deterministically by Slack thread', () => {
    const codex = resolveHarnessRollout({
      requestedHarness: 'codex',
      rolloutPercent: 50,
      threadId: 'slack:C1:1700000000.000100'
    })
    const nanocodex = resolveHarnessRollout({
      requestedHarness: 'codex',
      rolloutPercent: 50,
      threadId: 'slack:C1:1700000000.000104'
    })

    expect(codex.harnessType).toBe('codex')
    expect(nanocodex).toEqual({
      assignment: {
        experiment: 'codex_nanocodex_ab',
        requestedHarness: 'codex',
        cohort: 'nanocodex',
        rolloutPercent: 50
      },
      harnessType: 'nanocodex'
    })
    expect(
      resolveHarnessRollout({
        requestedHarness: 'codex',
        rolloutPercent: 50,
        threadId: 'slack:C1:1700000000.000104'
      })
    ).toEqual(nanocodex)
  })

  test('does not roll out a request carrying a non-default model', () => {
    expect(
      resolveHarnessRollout({
        modelOverride: 'gpt-5.5',
        requestedHarness: 'codex',
        rolloutPercent: 100,
        threadId: 'slack:C1:model-override'
      })
    ).toEqual({ harnessType: 'codex' })
  })

  test('honors rollout boundaries and leaves other harnesses alone', () => {
    expect(
      resolveHarnessRollout({
        requestedHarness: 'codex',
        rolloutPercent: 0,
        threadId: 'slack:C1:disabled'
      })
    ).toEqual({ harnessType: 'codex' })
    expect(
      resolveHarnessRollout({
        requestedHarness: 'codex',
        rolloutPercent: 100,
        threadId: 'slack:C1:complete'
      })
    ).toEqual({
      assignment: {
        experiment: 'codex_nanocodex_ab',
        requestedHarness: 'codex',
        cohort: 'nanocodex',
        rolloutPercent: 100
      },
      harnessType: 'nanocodex'
    })
    expect(
      resolveHarnessRollout({
        requestedHarness: 'claudecode',
        rolloutPercent: 50,
        threadId: 'slack:C1:claude'
      })
    ).toEqual({ harnessType: 'claudecode' })
  })

  test('is balanced across many Slack thread keys', () => {
    const nanocodex = Array.from({ length: 10_000 }, (_, index) => index).filter(index => {
      return (
        resolveHarnessRollout({
          requestedHarness: 'codex',
          rolloutPercent: 50,
          threadId: `slack:C1:rollout-${index}`
        }).harnessType === 'nanocodex'
      )
    }).length

    expect(nanocodex).toBeGreaterThanOrEqual(4_900)
    expect(nanocodex).toBeLessThanOrEqual(5_100)
  })
})
