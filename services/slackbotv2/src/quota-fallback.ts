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

const CLAUDE_SESSION_LIMIT_ANSWER =
  /^you(?:'|’)ve hit your session limit(?:\s*·\s*resets [^\r\n]{1,100})?$/iu

/**
 * Recognizes provider-generated terminal quota banners, not ordinary prose
 * that happens to discuss a quota or session limit.
 */
export function isProviderQuotaAnswer(answer: string): boolean {
  return CLAUDE_SESSION_LIMIT_ANSWER.test(answer.trim())
}

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
