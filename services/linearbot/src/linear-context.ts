import type { Logger } from "chat";
import type { LinearRawRequestClient } from "./types";
import { errorMessage, stringValue } from "./utils";

// Linear delta (no slackbotv2 analog; closest relative is discord-starter's
// thread-starter prepend): the thread history for an agent session is just the
// session's comment thread, which misses the issue itself (title, description,
// state, labels) and the rest of the issue conversation. Linear solves this
// for agents natively: every AgentSessionEvent webhook carries a
// `promptContext` blob — pre-formatted issue details + comments, curated by
// Linear — which the adapter exposes on the raw message. Prefer that; fall
// back to the message subject (one issue fetch) when it is absent (the
// Comment/Issue webhooks the comment-thread model runs on carry no
// promptContext, as do replayed/fetched history messages).

const CONTEXT_MAX_CHARS = 100_000;

// Linear delta: an assignment/delegation turn starts with NO user-written
// prompt. Without this, the empty execute message degrades to "" in the stored
// session and the literal "continue" on the codex input path. Synthesize an
// explicit instruction instead: the prepended issue context plus the overlay's
// system prompt are assumed sufficient to execute in some capacity. The
// ownership contract itself rides in OWNERSHIP_CONTEXT (injected into the
// context whenever the issue is owned), so it applies to comment turns on owned
// issues too — not just this empty-prompt case.
export const EMPTY_PROMPT_INSTRUCTION = [
  "You have been handed this Linear issue with no additional instructions.",
  "Review the issue context above and work the task to the best of your ability.",
].join("\n");

// Injected into the context whenever the issue is assigned or delegated to the
// bot — on assignment turns AND on comment turns where the delegate is the bot —
// so the agent knows it owns the work and may need to carry it forward, not just
// answer. The bot applies the terminal `Linear-Status:` marker as a backstop
// (see linear-status.ts); kickoff moves it to In Progress when work starts.
export const OWNERSHIP_CONTEXT = [
  "You own this Linear issue — it is assigned or delegated to you. Beyond answering this thread, carry the work forward and complete it if you can.",
  '- The issue is moved to "In Progress" automatically when you start work.',
  '- When you finish, use the `linear` CLI tool (if available) to move it to "Done" if the work is complete, or back to "Todo" if you could not make progress.',
  "- If you cannot update the issue with the tool, end your final answer with the line `Linear-Status: done`, `Linear-Status: todo`, or `Linear-Status: in_progress` and it will be applied for you.",
  "- If this looks like a recurring task, previous instances likely exist as other Linear issues; look them up with the `linear` tool for context and continuity.",
  "- Never delegate issues to yourself or mention yourself in comments.",
].join("\n");

const ISSUE_CONTEXT_QUERY = `
  query LinearbotIssueContext($issueId: String!) {
    issue(id: $issueId) {
      identifier
      title
      description
      url
      state { name }
      delegate { id }
    }
  }
`;

type IssueContextData = {
  issue?: {
    identifier?: unknown;
    title?: unknown;
    description?: unknown;
    url?: unknown;
    state?: { name?: unknown } | null;
    delegate?: { id?: unknown } | null;
  } | null;
};

export const ISSUE_CONTEXT_HEADER = "[Linear issue context]";

/** The issue an @-mention / assignment is about, reduced to context fields. */
export type LinearIssueContext = {
  identifier?: string;
  title?: string;
  description?: string;
  url?: string;
  status?: string;
  /** App-user id the issue is delegated to, when any — used for ownership. */
  delegateId?: string;
};

/**
 * Centaur-forward model: a Comment/Issue webhook carries no `promptContext`
 * blob, so the bot fetches the issue itself to tell the agent what it's working
 * on. Returns null on any failure or when the issue has no identifying fields
 * (never fail the turn); logs the reason so a persistent miss is diagnosable.
 */
export async function fetchLinearIssueContext(
  client: LinearRawRequestClient,
  issueId: string,
  logger: Logger,
): Promise<LinearIssueContext | null> {
  if (!client.client?.rawRequest) return null;
  let issue: IssueContextData["issue"];
  try {
    const response = await client.client.rawRequest<IssueContextData>(
      ISSUE_CONTEXT_QUERY,
      { issueId },
    );
    issue = response.data?.issue;
  } catch (error) {
    logger.warn("linearbot_issue_context_failed", {
      issue_id: issueId,
      error: errorMessage(error),
    });
    return null;
  }
  if (!issue) {
    logger.warn("linearbot_issue_context_empty", { issue_id: issueId });
    return null;
  }
  const context: LinearIssueContext = {
    identifier: stringValue(issue.identifier),
    title: stringValue(issue.title),
    description: stringValue(issue.description),
    url: stringValue(issue.url),
    status: issue.state?.name ? stringValue(issue.state.name) : undefined,
    delegateId: stringValue(issue.delegate?.id),
  };
  // Without an identifier or title there's nothing that tells the agent which
  // task this is — the whole point of the context.
  if (!context.identifier && !context.title) {
    logger.warn("linearbot_issue_context_insufficient", { issue_id: issueId });
    return null;
  }
  return context;
}

/**
 * Full issue context (identifier, title, status, url, description) — seeded on a
 * thread's first turn so the agent knows what the task is.
 */
export function formatIssueContext(
  context: LinearIssueContext,
  maxChars = CONTEXT_MAX_CHARS,
): string {
  const header = [context.identifier, context.title].filter(Boolean).join(": ");
  const facts = [
    context.status ? `Status: ${context.status}` : undefined,
    context.url ? `URL: ${context.url}` : undefined,
  ].filter(Boolean);
  const sections = [
    ISSUE_CONTEXT_HEADER,
    header,
    facts.join("\n"),
    context.description ? `Description:\n${context.description}` : undefined,
  ].filter(Boolean);
  return truncateContext(sections.join("\n\n"), maxChars);
}

/**
 * Compact one-line issue header (no description) — prepended on follow-up turns
 * so the agent always knows the task id/title, even if its sandbox lost the
 * fuller context that the first turn seeded.
 */
export function formatIssueContextHeader(context: LinearIssueContext): string {
  const name = [context.identifier, context.title].filter(Boolean).join(": ");
  return [
    `${ISSUE_CONTEXT_HEADER} ${name}`.trim(),
    context.status ? `(${context.status})` : undefined,
    context.url,
  ]
    .filter(Boolean)
    .join(" ");
}

function truncateContext(text: string, maxChars = CONTEXT_MAX_CHARS): string {
  if (text.length <= maxChars) return text;
  let omitted = text.length - maxChars;
  while (true) {
    const suffix = `\n[truncated ${omitted} chars from Linear issue context]`;
    const keep = Math.max(0, maxChars - suffix.length);
    const actualOmitted = text.length - keep;
    if (actualOmitted === omitted)
      return `${text.slice(0, keep).trimEnd()}${suffix}`;
    omitted = actualOmitted;
  }
}
