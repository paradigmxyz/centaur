import { createHash } from 'node:crypto'
import type { SlackbotV2HarnessAssignment } from './types'

const CODEX_NANOCODEX_AB_EXPERIMENT = 'codex_nanocodex_ab'

export type HarnessRolloutResolution = {
  assignment?: SlackbotV2HarnessAssignment
  harnessType: string
}

export function resolveHarnessRollout(input: {
  modelOverride?: string
  requestedHarness: string
  rolloutPercent: number
  threadId: string
}): HarnessRolloutResolution {
  if (
    input.requestedHarness !== 'codex' ||
    input.rolloutPercent <= 0 ||
    Boolean(input.modelOverride?.trim())
  ) {
    return { harnessType: input.requestedHarness }
  }

  const harnessType = selectCodexCohort(input.threadId, input.rolloutPercent)
  return {
    assignment: {
      experiment: CODEX_NANOCODEX_AB_EXPERIMENT,
      requestedHarness: input.requestedHarness,
      cohort: harnessType,
      rolloutPercent: input.rolloutPercent
    },
    harnessType
  }
}

function selectCodexCohort(threadId: string, nanocodexPercent: number): string {
  if (nanocodexPercent >= 100) return 'nanocodex'
  const bucket = createHash('sha256').update(threadId).digest().readUInt32BE(0)
  const threshold = (nanocodexPercent * 2 ** 32) / 100
  return bucket < threshold ? 'nanocodex' : 'codex'
}
