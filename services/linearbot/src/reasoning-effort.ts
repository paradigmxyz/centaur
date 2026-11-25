/**
 * Per-turn-type reasoning effort.
 *
 * The harness already accepts a per-turn effort: the blocks-protocol
 * `reasoning` field is mapped onto codex `turn/start.effort`, and slackbotv2
 * drives it from a `-rsn` flag. This bot's turns are mostly autonomous, though
 * — assignment kickoffs and comment replies on delegated issues — so there is
 * no human-authored message to put a flag into, and every turn ran at the
 * harness's global default.
 *
 * That default is a poor fit in both directions. An assignment turn
 * implementing a whole ticket wants deep thinking; a comment reply does not.
 * With one global setting a deployment chooses between saturating its inference
 * backend on assignment bursts and having implementation turns underthink.
 */

/** Turn kinds that can carry a distinct effort. */
export type TurnType = "assignment" | "comment";

/**
 * Codex reasoning efforts, plus the aliases slackbotv2 already accepts. Kept in
 * step with `services/slackbotv2/src/overrides.ts` so a value that works in a
 * `-rsn` flag also works in this config.
 */
const REASONING_EFFORTS: Record<string, string> = {
  none: "none",
  minimal: "minimal",
  min: "minimal",
  low: "low",
  medium: "medium",
  med: "medium",
  high: "high",
  hi: "high",
  xhigh: "xhigh",
  xhi: "xhigh",
  "x-high": "xhigh",
  max: "max",
};

/**
 * Normalizes a configured effort, returning undefined for anything
 * unrecognised.
 *
 * Unrecognised values are dropped rather than forwarded. A typo forwarded
 * verbatim reaches the harness as an invalid `turn/start.effort` and fails the
 * turn, which is a much worse outcome than running at the default — and the
 * config is set once at deploy time, where nobody is watching for it.
 */
export function normalizeReasoningEffort(value?: string): string | undefined {
  const key = value?.trim().toLowerCase();
  if (!key) return undefined;
  return REASONING_EFFORTS[key];
}

export type ReasoningEffortPolicy = {
  assignment?: string;
  comment?: string;
};

/** Resolves the configured effort for a turn type, if any is set and valid. */
export function reasoningEffortFor(
  policy: ReasoningEffortPolicy | undefined,
  turnType: TurnType,
): string | undefined {
  return normalizeReasoningEffort(policy?.[turnType]);
}
