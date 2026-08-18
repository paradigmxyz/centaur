/**
 * Linear chat-SDK thread keys (mirrors the adapter's encodeThreadId):
 *   linear:{issueId}                                  issue-level comments
 *   linear:{issueId}:c:{commentId}                    comment thread
 *   linear:{issueId}:s:{agentSessionId}               agent session on an issue
 *   linear:{issueId}:c:{commentId}:s:{agentSessionId} agent session on a comment
 *
 * The adapter is patched (patches/@chat-adapter__linear@4.30.0.patch) so every
 * message in one agent session resolves to the SAME stable thread key
 * (linear:{issueId}:s:{agentSessionId}); without that, each follow-up prompt
 * carried its own comment id in the key and spawned a fresh centaur session.
 */

export type LinearThreadKey = {
  agentSessionId?: string;
  commentId?: string;
  issueId?: string;
};

const COMMENT_SESSION_PATTERN = /^([^:]+):c:([^:]+):s:([^:]+)$/;
const ISSUE_SESSION_PATTERN = /^([^:]+):s:([^:]+)$/;
const COMMENT_PATTERN = /^([^:]+):c:([^:]+)$/;

export function parseLinearThreadKey(threadKey: string): LinearThreadKey {
  if (!threadKey.startsWith("linear:")) return {};
  const rest = threadKey.slice("linear:".length);
  if (!rest) return {};
  const commentSession = rest.match(COMMENT_SESSION_PATTERN);
  if (commentSession) {
    return {
      issueId: commentSession[1],
      commentId: commentSession[2],
      agentSessionId: commentSession[3],
    };
  }
  const issueSession = rest.match(ISSUE_SESSION_PATTERN);
  if (issueSession) {
    return { issueId: issueSession[1], agentSessionId: issueSession[2] };
  }
  const comment = rest.match(COMMENT_PATTERN);
  if (comment) {
    return { issueId: comment[1], commentId: comment[2] };
  }
  return { issueId: rest };
}

/** Agent session id for a thread, or undefined for plain comment threads. */
export function agentSessionIdFromThreadKey(
  threadKey: string,
): string | undefined {
  return parseLinearThreadKey(threadKey).agentSessionId;
}
