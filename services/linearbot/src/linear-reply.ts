import type { LinearRawRequestClient } from "./types";

// Linear delta (no slackbotv2 analog): in agent-sessions mode the agent's final
// answer is emitted as a `response` agent activity, which Linear renders in the
// detached agent-session widget on the issue — NOT inline in the comment thread
// the bot was asked in. Linear only mirrors a response into a thread when the
// session is anchored to a comment, and the agent-activity API gives us no way
// to anchor it ourselves (AgentActivityCreateInput carries no comment field;
// only user prompts carry sourceCommentId). So the bot posts the answer as a
// real comment reply itself, nested under the session's root comment — that is
// the surface humans actually read and reply in. The response activity still
// settles the session widget; this is the human-facing copy.

const COMMENT_CREATE_MUTATION = `
  mutation LinearbotCommentCreate(
    $issueId: String!
    $body: String!
    $parentId: String
  ) {
    commentCreate(
      input: { issueId: $issueId, body: $body, parentId: $parentId }
    ) {
      success
      comment {
        id
      }
    }
  }
`;

const COMMENT_UPDATE_MUTATION = `
  mutation LinearbotCommentUpdate($id: String!, $body: String!) {
    commentUpdate(id: $id, input: { body: $body }) {
      success
    }
  }
`;

/**
 * Posts a comment on an issue, nested under `parentCommentId` when given (a
 * reply in the session's comment thread) or top-level otherwise (delegated /
 * description-mention sessions, which have no comment thread). Returns the new
 * comment's id so the caller can live-edit it as the run streams. No-op (returns
 * undefined) when the client lacks the raw GraphQL escape hatch; callers treat
 * failures as best-effort, like narration — the durable answer is the response
 * activity.
 */
export async function postIssueReply(
  client: LinearRawRequestClient,
  input: { issueId: string; body: string; parentCommentId?: string },
): Promise<string | undefined> {
  if (!client.client?.rawRequest) return undefined;
  const response = await client.client.rawRequest<{
    commentCreate?: { comment?: { id?: unknown } | null } | null;
  }>(COMMENT_CREATE_MUTATION, {
    issueId: input.issueId,
    body: input.body,
    parentId: input.parentCommentId ?? null,
  });
  const id = response.data?.commentCreate?.comment?.id;
  return typeof id === "string" ? id : undefined;
}

/**
 * Edits an already-posted comment in place — used to update the live "Thinking…"
 * reply as the run streams and to swap it to the final answer when it settles.
 * No-op when the client lacks the raw GraphQL escape hatch; best-effort.
 */
export async function updateIssueReply(
  client: LinearRawRequestClient,
  input: { commentId: string; body: string },
): Promise<void> {
  if (!client.client?.rawRequest) return;
  await client.client.rawRequest(COMMENT_UPDATE_MUTATION, {
    id: input.commentId,
    body: input.body,
  });
}
