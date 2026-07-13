import type { GitHubAdapter } from "@chat-adapter/github";
import type { GithubbotOptions } from "./types";
import { errorMessage, noopLogger } from "./utils";

/**
 * Working/done/failed reaction acks for turns that have NO triggering comment to
 * react to — review requests, issue-work, and other lifecycle-driven turns. The
 * reaction lands on the PR/issue itself (its top post), so a teammate gets the
 * same instant 👀 → 🚀/😕 feedback they'd get from an @-mention. Additive, to
 * match the comment-reaction behavior (the 👀 isn't cleared on settle).
 *
 * All best-effort: a failed reaction never blocks or surfaces — it's only an ack.
 */
type Octokit = GitHubAdapter["octokit"];
type Logger = GithubbotOptions["logger"];

// A PR is an "issue" to the reactions API, so this covers both.
export async function reactWorkingOnSubject(
  octokit: Octokit,
  owner: string,
  repo: string,
  number: number,
  logger?: Logger,
): Promise<void> {
  try {
    await octokit.rest.reactions.createForIssue({
      owner,
      repo,
      issue_number: number,
      content: "eyes",
    });
  } catch (error) {
    (logger ?? noopLogger).debug("githubbot_subject_react_failed", {
      error: errorMessage(error),
    });
  }
}

export async function settleSubjectReaction(
  octokit: Octokit,
  owner: string,
  repo: string,
  number: number,
  failed: boolean,
  logger?: Logger,
): Promise<void> {
  try {
    await octokit.rest.reactions.createForIssue({
      owner,
      repo,
      issue_number: number,
      content: failed ? "confused" : "rocket",
    });
  } catch (error) {
    (logger ?? noopLogger).debug("githubbot_subject_react_settle_failed", {
      error: errorMessage(error),
    });
  }
}
