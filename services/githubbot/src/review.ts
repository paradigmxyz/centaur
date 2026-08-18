import type { GitHubAdapter } from "@chat-adapter/github";
import type { StateAdapter } from "chat";
import { backgroundWaitUntil } from "./context";
import { reactWorkingOnSubject, settleSubjectReaction } from "./reactions";
import { DEFAULT_REVIEW_PROMPT } from "./review-prompt";
import { runTurnStream } from "./turn";
import type {
  ForwardSessionInput,
  GithubbotApiMessage,
  GithubbotOptions,
  GithubbotTrace,
} from "./types";
import { errorMessage, noopLogger, nowMs, stringValue, traceLog } from "./utils";

type ReviewHandlerInput = {
  botUserName: string;
  deliveryId: string;
  octokit: GitHubAdapter["octokit"];
  options: GithubbotOptions;
  state: StateAdapter;
};

type PullRequestWebhookPayload = {
  action?: unknown;
  pull_request?: {
    head?: { sha?: unknown };
    html_url?: unknown;
    number?: unknown;
    state?: unknown;
    title?: unknown;
  };
  repository?: { full_name?: unknown };
  requested_reviewer?: { login?: unknown; type?: unknown };
  requested_team?: { slug?: unknown; name?: unknown };
  sender?: { login?: unknown };
};

// Review-request webhooks are de-duplicated by delivery id for a week — long
// enough to cover GitHub's redelivery window without growing state unboundedly.
const REVIEW_DEDUP_TTL_MS = 7 * 24 * 60 * 60 * 1000;
// Team membership is cached briefly so a flurry of team review-requests doesn't
// hit the API for every one.
const TEAM_MEMBERSHIP_CACHE_TTL_MS = 10 * 60 * 1000;

/**
 * Review-on-request trigger. The GitHub chat adapter only surfaces comment
 * threads, so the `pull_request` lifecycle event (action `review_requested`)
 * arrives as a raw webhook we handle here: when the bot's teammate account is
 * the requested reviewer, run a review turn.
 *
 * The review runs on its own isolated session thread (`github-review:{owner}/
 * {repo}:{number}`), kept separate from the PR's conversation thread so a review
 * never shares a sandbox/context with chit-chat — but persistent per PR, so a
 * re-request builds on the prior review instead of starting cold. The agent
 * posts inline comments and a summary itself via `gh` (per the review prompt),
 * so the bot does not post through the adapter. The methodology is the bundled
 * default unless the deployment fully replaces it via options.reviewPrompt.
 * Returns null when the webhook is not a review-request for us.
 */
export function handleReviewRequest(
  rawBody: string,
  input: ReviewHandlerInput,
): Promise<void> | null {
  let payload: PullRequestWebhookPayload;
  try {
    payload = JSON.parse(rawBody) as PullRequestWebhookPayload;
  } catch {
    return null;
  }
  if (payload.action !== "review_requested") return null;
  const repoFullName = stringValue(payload.repository?.full_name);
  const number = numberValue(payload.pull_request?.number);
  if (!repoFullName || number === undefined) return null;
  const [owner, repo] = repoFullName.split("/", 2);
  if (!owner || !repo) return null;

  // The request is ours when the bot is the requested reviewer, or when the bot
  // belongs to a requested team (resolved asynchronously below). A request that
  // names a different individual reviewer and no team is not ours.
  const reviewer = stringValue(payload.requested_reviewer?.login);
  const directMatch =
    !!reviewer && reviewer.toLowerCase() === input.botUserName.toLowerCase();
  const teamSlug = stringValue(payload.requested_team?.slug);
  if (!directMatch && !teamSlug) return null;

  const reviewThreadKey = `github-review:${owner}/${repo}:${number}`;
  const title = stringValue(payload.pull_request?.title) ?? `#${number}`;
  const url =
    stringValue(payload.pull_request?.html_url) ??
    `https://github.com/${owner}/${repo}/pull/${number}`;
  const headSha = stringValue(payload.pull_request?.head?.sha) ?? "head";
  const requester = stringValue(payload.sender?.login) ?? "a teammate";
  const { options, state } = input;

  const trace: GithubbotTrace = {
    includeContext: false,
    messageId: `review-${reviewThreadKey}-${input.deliveryId}`,
    mode: "execute",
    openStream: true,
    startedAtMs: nowMs(),
    threadId: reviewThreadKey,
  };

  return (async () => {
    const logger = options.logger ?? noopLogger;
    // A team review request only counts when the bot is actually a member of the
    // requested team (a direct request of the bot needs no such check).
    if (!directMatch && teamSlug && !(await isBotOnTeam(input, owner, teamSlug))) {
      traceLog(options, "githubbot_review_team_not_member_skipped", trace, {
        team: `${owner}/${teamSlug}`,
      });
      return;
    }
    // Claim the delivery before the background run so a redelivery never
    // double-reviews. State-keyed (not Chat-thread-keyed) because the review
    // thread is synthetic and never touches the adapter.
    const dedupKey = `${options.stateKeyPrefix ?? "centaur-githubbot"}:review-delivery:${reviewThreadKey}:${input.deliveryId}`;
    let claimed = true;
    try {
      claimed = await state.setIfNotExists(dedupKey, "1", REVIEW_DEDUP_TTL_MS);
    } catch (error) {
      logger.debug("githubbot_review_dedup_failed", {
        error: errorMessage(error),
      });
    }
    if (!claimed) {
      traceLog(options, "githubbot_review_duplicate_skipped", trace, {
        delivery_id: input.deliveryId,
      });
      return;
    }
    traceLog(options, "githubbot_review_requested", trace, {
      pr: `${owner}/${repo}#${number}`,
      requester,
    });
    // There's no triggering comment to react to (a review request is a lifecycle
    // event), so ack on the PR itself — instant 👀, settled to 🚀/😕 below.
    await reactWorkingOnSubject(input.octokit, owner, repo, number, logger);

    let lastEventId = 0;
    const forwardInput: ForwardSessionInput = {
      afterEventId: 0,
      // The full review methodology rides as the context preamble; a deployment
      // can fully replace it via options.reviewPrompt (the override is used
      // verbatim, so org conventions supersede ours wholesale).
      contextPreamble: options.reviewPrompt ?? DEFAULT_REVIEW_PROMPT,
      conversationName: `${owner}/${repo}#${number}: ${title}`,
      executeMessage: reviewTriggerMessage({
        headSha,
        number,
        owner,
        repo,
        requester,
        threadKey: reviewThreadKey,
        title,
        url,
      }),
      messages: [],
      model: undefined,
      onEventId: (eventId) => {
        lastEventId = Math.max(lastEventId, eventId);
        forwardInput.afterEventId = lastEventId;
      },
      openStream: false,
      threadId: reviewThreadKey,
      trace,
    };

    backgroundWaitUntil(
      runTurnStream(options, forwardInput)
        .then(async (result) => {
          traceLog(options, "githubbot_review_turn_complete", trace, {
            failed: result.failed,
          });
          await settleSubjectReaction(
            input.octokit,
            owner,
            repo,
            number,
            result.failed,
            logger,
          );
        })
        .catch(async (error) => {
          logger.warn("githubbot_review_turn_failed", {
            error: errorMessage(error),
          });
          await settleSubjectReaction(
            input.octokit,
            owner,
            repo,
            number,
            true,
            logger,
          );
        }),
    );
  })();
}

/**
 * Whether the bot's account belongs to `{org}/{teamSlug}`, cached briefly. A
 * 404 (not a member, or the team isn't visible to the token) is treated as "not
 * ours" so an unrelated team's review request is ignored.
 */
async function isBotOnTeam(
  input: ReviewHandlerInput,
  org: string,
  teamSlug: string,
): Promise<boolean> {
  const cacheKey = `${input.options.stateKeyPrefix ?? "centaur-githubbot"}:team-member:${org}/${teamSlug}:${input.botUserName.toLowerCase()}`;
  try {
    const cached = await input.state.get<string>(cacheKey);
    if (cached === "1") return true;
    if (cached === "0") return false;
  } catch {
    // fall through to a live lookup
  }
  let member = false;
  try {
    const { data } = await input.octokit.rest.teams.getMembershipForUserInOrg({
      org,
      team_slug: teamSlug,
      username: input.botUserName,
    });
    member = data.state === "active";
  } catch (error) {
    (input.options.logger ?? noopLogger).debug(
      "githubbot_team_membership_lookup_failed",
      { error: errorMessage(error), team: `${org}/${teamSlug}` },
    );
    member = false;
  }
  try {
    await input.state.set(
      cacheKey,
      member ? "1" : "0",
      TEAM_MEMBERSHIP_CACHE_TTL_MS,
    );
  } catch {
    // best-effort cache
  }
  return member;
}

/**
 * The specific ask for a review-request turn (the methodology rides separately
 * as the context preamble). Keyed by head sha so re-requesting review on a new
 * commit re-executes (the session idempotency key dedupes the same commit).
 */
function reviewTriggerMessage(input: {
  headSha: string;
  number: number;
  owner: string;
  repo: string;
  requester: string;
  threadKey: string;
  title: string;
  url: string;
}): GithubbotApiMessage {
  const text =
    `You have been requested to review pull request ` +
    `${input.owner}/${input.repo}#${input.number} — "${input.title}" ` +
    `(${input.url}), at commit ${input.headSha}, by @${input.requester}. ` +
    `Follow your review guidance above and post your review now using the gh ` +
    `CLI in your sandbox.`;
  return {
    attachments: [],
    author: {
      fullName: "GitHub",
      isBot: false,
      isMe: false,
      userId: "github-review",
      userName: "github-review",
    },
    id: `review-${input.threadKey}-${input.headSha}`,
    isMention: true,
    raw: { githubbotReviewRequest: true, url: input.url },
    text,
    threadId: input.threadKey,
    timestamp: new Date().toISOString(),
  };
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}
