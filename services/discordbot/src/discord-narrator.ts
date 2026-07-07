import type { ChatSDKStreamChunk } from "@centaur/rendering";
import type { Logger, Thread } from "chat";
import { parseDiscordThreadKey } from "./discord-allowlist";
import { DEFAULT_DISCORD_API_URL } from "./discord-threading";
import type { DiscordbotApiMessage, DiscordbotOptions } from "./types";
import { errorMessage, nowMs, sliceSurrogateSafe } from "./utils";

export type DiscordNarratorChunk = Exclude<
  ChatSDKStreamChunk,
  { type: "markdown_text" }
>;
type DiscordTaskChunk = Extract<ChatSDKStreamChunk, { type: "task_update" }>;

/** Terminal state the run's reaction settles into. */
export type DiscordNarratorOutcome = "done" | "failed" | "retrying";

const REACTION_WORKING = "👀";
const REACTION_DONE = "✅";
const REACTION_FAILED = "❌";

// Discord caps message content at 2000 chars; headroom keeps every post safe.
const NARRATOR_MESSAGE_MAX_CHARS = 1_900;
// A single blurb is truncated to this, and a thought still pending at this size
// is flushed early so long reasoning doesn't sit invisible for the whole run.
const NARRATOR_BLURB_MAX_CHARS = 600;
// Thoughts that complete within this window merge into one message; also keeps
// posts well under Discord's per-channel message budget.
const NARRATOR_MIN_POST_GAP_MS = 1_500;
// Runaway runs stop narrating past this many posted messages.
const NARRATOR_MAX_POSTS = 12;
// Fragments shorter than this aren't worth a message of their own.
const NARRATOR_MIN_BLURB_CHARS = 12;

export type DiscordNarratorOptions = {
  logger: Logger;
  maxPosts?: number;
  minPostGapMs?: number;
};

/**
 * The Discord-side chain-of-thought surface, fully append-only: the triggering
 * message gets an instant 👀 reaction while the agent works, the agent's
 * reasoning blurbs post as their own subtext (-#) messages as each thought
 * completes, and on settle the 👀 is swapped for ✅ (or ❌). No bot message is
 * ever edited or deleted. Commands, tools, and plan updates are not rendered;
 * they just mark where a thought ends.
 *
 * Reactions go through the raw Discord REST API rather than the adapter: a
 * thread-starter message lives in the PARENT channel (same delta that
 * motivates discord-starter.ts), while the adapter always routes reactions to
 * the thread.
 */
export class DiscordNarrator {
  private readonly thread: Thread;
  private readonly botOptions: DiscordbotOptions;
  private readonly logger: Logger;
  private readonly minPostGapMs: number;
  private readonly maxPosts: number;
  private readonly reactionChannelId: string | undefined;
  private readonly reactionMessageId: string;
  // Current thought, keyed by chunk id: reasoning deltas have unique ids and
  // concatenate; a commentary item re-uses its id and replaces its body.
  private pendingParts = new Map<string, string>();
  private queuedBlurbs: string[] = [];
  private lastStatus = "";
  private postedCount = 0;
  private droppedBlurbs = 0;
  private lastPostAtMs = 0;
  private sawError = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private chain: Promise<void> = Promise.resolve();
  private finished = false;

  private constructor(
    thread: Thread,
    message: DiscordbotApiMessage,
    botOptions: DiscordbotOptions,
    options: DiscordNarratorOptions,
  ) {
    this.thread = thread;
    this.botOptions = botOptions;
    this.logger = options.logger;
    this.minPostGapMs = options.minPostGapMs ?? NARRATOR_MIN_POST_GAP_MS;
    this.maxPosts = options.maxPosts ?? NARRATOR_MAX_POSTS;
    const { channelId, threadId } = parseDiscordThreadKey(thread.id);
    // A thread-starter message (id == thread id) lives in the parent channel;
    // anything else lives in the thread itself.
    this.reactionChannelId =
      message.id === threadId ? channelId : (threadId ?? channelId);
    this.reactionMessageId = message.id;
  }

  /** Adds the 👀 working reaction (best-effort) and returns the narrator. */
  static start(
    thread: Thread,
    message: DiscordbotApiMessage,
    botOptions: DiscordbotOptions,
    options: DiscordNarratorOptions,
  ): DiscordNarrator {
    const narrator = new DiscordNarrator(thread, message, botOptions, options);
    narrator.enqueueReaction("PUT", REACTION_WORKING);
    return narrator;
  }

  /**
   * Server-side activity summaries (renderer.status events) — the Discord
   * analog of Slack's assistant status. Discord has no ephemeral status
   * surface, so summaries post as append-only subtext blurbs, like thoughts
   * did before reasoning synthesis moved out of the renderer. Empty statuses
   * (the end-of-run clear) and consecutive repeats are dropped.
   */
  status(text: string): void {
    if (this.finished) return;
    const trimmed = text.trim();
    if (!trimmed || trimmed === this.lastStatus) return;
    this.lastStatus = trimmed;
    if (trimmed.length < NARRATOR_MIN_BLURB_CHARS) return;
    this.queuedBlurbs.push(truncateBlurb(trimmed));
    this.schedulePost();
  }

  update(chunk: DiscordNarratorChunk): void {
    if (this.finished) return;
    if (chunk.type !== "task_update") return;
    if (chunk.status === "error") this.sawError = true;
    if (chunk.title === "Thinking") {
      if (chunk.details) this.pendingParts.set(chunk.id, chunk.details);
      if (
        chunk.status === "complete" ||
        this.pendingText().length >= NARRATOR_BLURB_MAX_CHARS
      ) {
        this.flushPending();
      }
      return;
    }
    // Any other task means the model moved on — the current thought is over.
    this.flushPending();
  }

  /**
   * Posts any remaining thought, then settles the reaction: ✅ on success,
   * ❌ on failure, and 👀 stays put for "retrying" (the retry attempt re-adds
   * it; the PUT is idempotent). Never throws — narration is cosmetic. A "done"
   * outcome downgrades to "failed" when an error task was seen (the renderer
   * surfaces in-stream failures as error tasks, not throws).
   */
  async finish(outcome: DiscordNarratorOutcome): Promise<void> {
    if (this.finished) return;
    this.finished = true;
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.flushPendingText();
    this.enqueueBlurbPost();
    const failed =
      outcome === "failed" || (outcome === "done" && this.sawError);
    if (outcome !== "retrying") {
      // Add the settled reaction before clearing 👀 so the message always
      // carries an indicator.
      this.enqueueReaction("PUT", failed ? REACTION_FAILED : REACTION_DONE);
      this.enqueueReaction("DELETE", REACTION_WORKING);
    }
    await this.chain;
    if (this.droppedBlurbs) {
      this.logger.debug("discordbot_narrator_blurbs_dropped", {
        dropped: this.droppedBlurbs,
      });
    }
  }

  private pendingText(): string {
    return Array.from(this.pendingParts.values()).join("").trim();
  }

  private flushPending(): void {
    this.flushPendingText();
    this.schedulePost();
  }

  private flushPendingText(): void {
    const text = this.pendingText();
    this.pendingParts = new Map();
    if (text.length < NARRATOR_MIN_BLURB_CHARS) return;
    this.queuedBlurbs.push(truncateBlurb(text));
  }

  private schedulePost(): void {
    if (this.timer || !this.queuedBlurbs.length) return;
    const delayMs = Math.max(
      0,
      this.minPostGapMs - (nowMs() - this.lastPostAtMs),
    );
    this.timer = setTimeout(() => {
      this.timer = null;
      this.enqueueBlurbPost();
    }, delayMs);
  }

  private enqueueBlurbPost(): void {
    if (!this.queuedBlurbs.length) return;
    if (this.postedCount >= this.maxPosts) {
      this.droppedBlurbs += this.queuedBlurbs.length;
      this.queuedBlurbs = [];
      return;
    }
    const blurbs = this.queuedBlurbs;
    this.queuedBlurbs = [];
    this.postedCount += 1;
    this.lastPostAtMs = nowMs();
    const content = clipMessage(
      blurbs.map((blurb) => subtext(blurb)).join("\n\n"),
    );
    this.chain = this.chain.then(async () => {
      try {
        // `raw` skips the SDK's markdown round-trip, which would escape the
        // leading -# and break Discord's subtext rendering.
        await this.thread.adapter.postMessage(this.thread.id, {
          raw: content,
        });
      } catch (error) {
        this.logger.warn("discordbot_narrator_post_failed", {
          error: errorMessage(error),
        });
      }
    });
  }

  private enqueueReaction(method: "PUT" | "DELETE", emoji: string): void {
    const channelId = this.reactionChannelId;
    if (!channelId) return;
    this.chain = this.chain.then(() =>
      discordReactionRequest(
        this.botOptions,
        channelId,
        { emoji, messageId: this.reactionMessageId, method },
        this.logger,
      ),
    );
  }
}

/**
 * Discord delta (no slackbotv2 analog, shared by the narrator and the ingress
 * guards): best-effort reaction via the raw Discord REST API, parent-channel
 * aware — a thread-starter message (id == thread segment) lives in the parent
 * channel, while the adapter always routes reactions to the thread.
 */
export async function reactToDiscordMessage(
  botOptions: DiscordbotOptions,
  input: {
    emoji: string;
    messageId: string;
    method?: "PUT" | "DELETE";
    threadKey: string;
  },
  logger: Logger,
): Promise<void> {
  const { channelId, threadId } = parseDiscordThreadKey(input.threadKey);
  const targetChannelId =
    input.messageId === threadId ? channelId : (threadId ?? channelId);
  if (!targetChannelId) return;
  await discordReactionRequest(
    botOptions,
    targetChannelId,
    {
      emoji: input.emoji,
      messageId: input.messageId,
      method: input.method ?? "PUT",
    },
    logger,
  );
}

/** Raw REST reaction request; never throws (reactions are cosmetic). */
async function discordReactionRequest(
  botOptions: DiscordbotOptions,
  channelId: string,
  input: { emoji: string; messageId: string; method: "PUT" | "DELETE" },
  logger: Logger,
): Promise<void> {
  const { emoji, messageId, method } = input;
  try {
    const fetchFn = botOptions.fetch ?? fetch;
    const apiBase = (
      botOptions.discordApiUrl ?? DEFAULT_DISCORD_API_URL
    ).replace(/\/$/, "");
    const response = await fetchFn(
      `${apiBase}/channels/${channelId}/messages/${messageId}/reactions/${encodeURIComponent(emoji)}/@me`,
      {
        method,
        headers: { authorization: `Bot ${botOptions.botToken}` },
      },
    );
    if (!response.ok) {
      logger.warn("discordbot_narrator_reaction_failed", {
        emoji,
        method,
        status: response.status,
      });
    }
  } catch (error) {
    logger.warn("discordbot_narrator_reaction_error", {
      emoji,
      method,
      error: errorMessage(error),
    });
  }
}

/** Discord subtext is a per-line prefix, so every non-empty line needs it. */
function subtext(text: string): string {
  return text
    .split("\n")
    .map((line) => (line.trim() ? `-# ${line.trim()}` : ""))
    .join("\n")
    .replace(/\n{3,}/g, "\n\n");
}

// Discord delta: surrogate-safe cuts — slicing raw UTF-16 units can halve an
// emoji's surrogate pair, which Discord rejects with a 400.
function truncateBlurb(text: string): string {
  if (text.length <= NARRATOR_BLURB_MAX_CHARS) return text;
  return `${sliceSurrogateSafe(text, NARRATOR_BLURB_MAX_CHARS - 1).trimEnd()}…`;
}

function clipMessage(content: string): string {
  if (content.length <= NARRATOR_MESSAGE_MAX_CHARS) return content;
  return sliceSurrogateSafe(content, NARRATOR_MESSAGE_MAX_CHARS);
}
