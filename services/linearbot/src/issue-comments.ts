import { isJsonObject, stringValue } from "./utils";

// Linear delta (no slackbotv2 analog): in agent-sessions mode the adapter
// ignores `Comment` webhooks entirely, so a delegated agent never saw regular
// comments posted on its issue outside the session thread ("actually, hold
// off"). linearbot routes comment-created webhooks for issues with known
// agent-session threads into those threads as append-only context — no
// execution, exactly like a non-mention subscribed message. Comments that are
// part of a session's own thread already arrive as `prompted` events and are
// skipped here.

/** A comment-created webhook, reduced to the fields the forwarder needs. */
export type IssueCommentEvent = {
  authorId: string;
  authorName: string;
  body: string;
  commentId: string;
  createdAt?: string;
  issueId: string;
  parentId?: string;
  url?: string;
};

/**
 * Parses a Linear `Comment`/`create` webhook body into an IssueCommentEvent.
 * Returns null for anything else — including bot/agent-authored comments
 * (those carry a `botActor` instead of a `user`), which keeps the agent's own
 * response comments from echoing back into its session.
 */
export function parseIssueCommentWebhook(
  rawBody: string,
): IssueCommentEvent | null {
  let payload: unknown;
  try {
    payload = JSON.parse(rawBody);
  } catch {
    return null;
  }
  if (!isJsonObject(payload)) return null;
  if (payload.type !== "Comment" || payload.action !== "create") return null;
  const data = payload.data;
  if (!isJsonObject(data)) return null;
  const issueId = stringValue(data.issueId);
  const commentId = stringValue(data.id);
  const body = typeof data.body === "string" ? data.body : "";
  const user = isJsonObject(data.user) ? data.user : undefined;
  const authorId = stringValue(user?.id);
  if (!issueId || !commentId || !authorId || !body.trim()) return null;
  return {
    authorId,
    authorName: stringValue(user?.name) ?? "unknown",
    body,
    commentId,
    createdAt: stringValue(data.createdAt),
    issueId,
    parentId: stringValue(data.parentId),
    url: stringValue(payload.url),
  };
}

/** An issue handed to the bot, reduced to what the assignment turn needs. */
export type IssueAssignmentEvent = {
  issueId: string;
  /** True when the bot is the issue's delegate (vs. plain assignee). */
  delegated: boolean;
  /** Issue `updatedAt`; dedupes a redelivered webhook for the same change. */
  updatedAt: string;
};

/**
 * Parses an `Issue` webhook into an IssueAssignmentEvent when the issue was just
 * handed to `botUserId` — assigned OR delegated — and should be worked. Returns
 * null otherwise. The Centaur-forward model uses this (not an AgentSessionEvent)
 * so handoff turns survive agent sessions being off.
 *
 * - `create`: fires whenever the new issue's assignee/delegate is the bot — the
 *   handoff is inherent to creation, and there's no `updatedFrom` to gate on.
 * - `update`: fires only when the field pointing at the bot actually CHANGED in
 *   this update. Linear lists the prior values of changed fields in
 *   `updatedFrom`; if it's present but lacks the relevant field, this was an
 *   unrelated edit (a label, a description, or the bot's own status write
 *   bouncing back) and must not re-run the agent. When `updatedFrom` is absent
 *   we fall back to the membership check alone, to stay robust.
 * - Never fires when the webhook's `actor` is the bot itself: a handoff turn
 *   exists to pick up work someone GAVE the bot, and the bot self-assigning
 *   mid-turn (a natural "I'm taking this" tool call) must not spawn a second
 *   turn on work already underway. When `actor` is absent (older payload
 *   shapes) we keep the prior fire-on-membership behavior.
 */
export function parseIssueAssignmentWebhook(
  rawBody: string,
  botUserId: string,
): IssueAssignmentEvent | null {
  let payload: unknown;
  try {
    payload = JSON.parse(rawBody);
  } catch {
    return null;
  }
  if (!isJsonObject(payload)) return null;
  if (payload.type !== "Issue") return null;
  const action = payload.action;
  if (action !== "create" && action !== "update") return null;
  const data = payload.data;
  if (!isJsonObject(data)) return null;
  const issueId = stringValue(data.id);
  if (!issueId) return null;
  const assignedToBot = stringValue(data.assigneeId) === botUserId;
  const delegatedToBot = stringValue(data.delegateId) === botUserId;
  if (!assignedToBot && !delegatedToBot) return null;
  const actor = isJsonObject(payload.actor) ? payload.actor : undefined;
  if (actor && stringValue(actor.id) === botUserId) return null;
  if (action === "update") {
    const updatedFrom = isJsonObject(payload.updatedFrom)
      ? payload.updatedFrom
      : isJsonObject(data.updatedFrom)
        ? data.updatedFrom
        : undefined;
    if (updatedFrom) {
      const assigneeChanged = assignedToBot && "assigneeId" in updatedFrom;
      const delegateChanged = delegatedToBot && "delegateId" in updatedFrom;
      if (!assigneeChanged && !delegateChanged) return null;
    }
  }
  return {
    issueId,
    delegated: delegatedToBot,
    updatedAt:
      stringValue(data.updatedAt) ?? stringValue(payload.updatedAt) ?? "",
  };
}
