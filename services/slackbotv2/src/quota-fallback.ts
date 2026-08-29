const SUPPORTED_HARNESS_TYPES = new Set([
  'amp',
  'claudecode',
  'codex',
  'hermes',
  'nanocodex'
])

export type QuotaFallbackDecision =
  | { outcome: 'disabled' }
  | { harnessType: string; outcome: 'misconfigured' }
  | { outcome: 'suppressed_already_attempted' }
  | { outcome: 'suppressed_explicit_harness' }
  | { outcome: 'suppressed_same_harness' }
  | { harnessType: string; outcome: 'scheduled' }

export function quotaFallbackDecision(input: {
  alreadyAttempted: boolean
  configuredFallbackHarness?: string
  defaultHarness: string
  explicitHarnessRequested: boolean
  failedHarness: string
}): QuotaFallbackDecision {
  const configuredFallbackHarness = input.configuredFallbackHarness
  if (!configuredFallbackHarness) return { outcome: 'disabled' }
  if (!SUPPORTED_HARNESS_TYPES.has(configuredFallbackHarness)) {
    return { harnessType: configuredFallbackHarness, outcome: 'misconfigured' }
  }
  if (input.explicitHarnessRequested) {
    return { outcome: 'suppressed_explicit_harness' }
  }
  if (input.alreadyAttempted) {
    return { outcome: 'suppressed_already_attempted' }
  }

  const harnessType =
    input.failedHarness === configuredFallbackHarness
      ? input.defaultHarness
      : configuredFallbackHarness
  if (!SUPPORTED_HARNESS_TYPES.has(harnessType)) {
    return { harnessType, outcome: 'misconfigured' }
  }
  if (harnessType === input.failedHarness) {
    return { outcome: 'suppressed_same_harness' }
  }
  return { harnessType, outcome: 'scheduled' }
}
