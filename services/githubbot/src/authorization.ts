import type { GithubbotOptions } from "./types";

/**
 * Authorization gate for the conversational (comment) path. A comment that
 * @-mentions the bot drives an agent turn in a write-capable sandbox and posts a
 * transcript back, so only sufficiently-trusted authors may trigger it —
 * otherwise any commenter (anyone on a public repo) could steer the agent and
 * read back its tool output. The lifecycle paths (assignment, review-request)
 * are already gated by GitHub permissions, so this applies only to comment
 * mentions and their follow-ups.
 *
 * GitHub stamps every comment with an `author_association` (OWNER / MEMBER /
 * COLLABORATOR / CONTRIBUTOR / FIRST_TIME_CONTRIBUTOR / NONE / …). We trust the
 * collaborator-and-up set by default; a deployment can widen or narrow it, and
 * the sentinel "*" allows everyone (e.g. a fully-private repo where every
 * commenter is already trusted).
 */
export const DEFAULT_ALLOWED_AUTHOR_ASSOCIATIONS = [
  "OWNER",
  "MEMBER",
  "COLLABORATOR",
] as const;

const ALLOW_ALL = "*";

/** Pull `author_association` out of the adapter's raw comment message. */
export function authorAssociationFromRaw(raw: unknown): string | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const comment = (raw as { comment?: unknown }).comment;
  if (!comment || typeof comment !== "object") return undefined;
  const value = (comment as { author_association?: unknown }).author_association;
  return typeof value === "string" ? value : undefined;
}

/** Normalize the configured allowlist, defaulting when unset or empty. */
export function resolveAllowedAuthorAssociations(
  configured: readonly string[] | undefined,
): string[] {
  const list = (configured ?? DEFAULT_ALLOWED_AUTHOR_ASSOCIATIONS)
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map((entry) => entry.toUpperCase());
  return list.length ? list : [...DEFAULT_ALLOWED_AUTHOR_ASSOCIATIONS];
}

/**
 * Whether a comment's author is allowed to drive a turn. Fails closed: an
 * association we can't read (an unexpected payload shape) is treated as
 * untrusted rather than waved through.
 */
export function isCommentAuthorAllowed(
  raw: unknown,
  options: Pick<GithubbotOptions, "allowedAuthorAssociations">,
): boolean {
  const allowed = resolveAllowedAuthorAssociations(
    options.allowedAuthorAssociations,
  );
  if (allowed.includes(ALLOW_ALL)) return true;
  const association = authorAssociationFromRaw(raw);
  if (!association) return false;
  return allowed.includes(association.toUpperCase());
}
