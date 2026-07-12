import type { ChatSDKStreamChunk } from "@centaur/rendering";
import { TelegramApiError } from "./telegram-api";
import type { SendMessageParams, TelegramApi } from "./telegram-api";
import type { Logger, TelegramNarratorOutcome } from "./types";
import { errorMessage, nowMs, sliceSurrogateSafe } from "./utils";

export type TelegramNarratorChunk = Exclude<
  ChatSDKStreamChunk,
  { type: "markdown_text" }
>;

// Telegram delta: setMessageReaction accepts only the Bot API's fixed
// ReactionTypeEmoji whitelist — the Discord-inherited ✅/❌ are NOT in it and
// the real API rejects them with 400 REACTION_INVALID on every settle, so
// done/failed map to the whitelisted 👍/👎 (👀 is whitelisted and stays).
const REACTION_WORKING = "👀";
const REACTION_DONE = "👍";
const REACTION_FAILED = "👎";

// Telegram delta: sendMessage allows 4096 chars *after entity parsing*, so the
// budget below is measured on the visible (unescaped) text and the HTML
// tags/escapes ride free; 3900 keeps every post conservatively under the cap.
const NARRATOR_MESSAGE_MAX_CHARS = 3_900;
// A single blurb is truncated to this, and a thought still pending at this size
// is flushed early so long reasoning doesn't sit invisible for the whole run.
const NARRATOR_BLURB_MAX_CHARS = 600;
// Thoughts that complete within this window merge into one message; also keeps
// posts well inside Telegram's ~1 msg/s + 20/min-per-group send guidance.
const NARRATOR_MIN_POST_GAP_MS = 1_500;
// Runaway runs stop narrating past this many posted messages.
const NARRATOR_MAX_POSTS = 12;
// Fragments shorter than this aren't worth a message of their own.
const NARRATOR_MIN_BLURB_CHARS = 12;
// sendChatAction("typing") lights the indicator for ~5s, so the keepalive
// re-sends every 4s measured from the START of each send: the send itself can
// ride the per-chat FIFO behind paced messages for a second or more, and a
// full 5s sleep after it completes would let the indicator lapse and flicker.
const TYPING_KEEPALIVE_INTERVAL_MS = 4_000;

/** Injectable time source so tests drive the typing cadence deterministically
 * (same seam as rate-limit's RateLimitClock, declared locally because the
 * narrator must not depend on rate-limit.ts). */
export type TelegramNarratorClock = {
  now(): number;
  sleep(ms: number): Promise<void>;
};

/**
 * Where narration lands. Telegram delta (vs the Discord Chat SDK Thread): the
 * narrator addresses the chat directly — blurbs go to `chatId` (staying in
 * `messageThreadId` when the conversation is a forum/private topic), and
 * reactions target the triggering message, which needs no topic id because it
 * already lives in one.
 */
export type TelegramNarratorTarget = {
  chatId: number | string;
  messageThreadId?: number | null;
  triggerMessageId: number;
};

export type TelegramNarratorOptions = {
  logger: Logger;
  clock?: TelegramNarratorClock;
  maxPosts?: number;
  minPostGapMs?: number;
  typingIntervalMs?: number;
};

/**
 * The Telegram-side chain-of-thought surface, fully append-only: the
 * triggering message gets an instant 👀 reaction while the agent works, the
 * agent's reasoning blurbs post as their own italic messages as each thought
 * completes, and on settle the reaction becomes 👍 (or 👎). No bot message is
 * ever edited or deleted. Commands, tools, and plan updates are not rendered;
 * they just mark where a thought ends.
 *
 * Telegram deltas vs discord-narrator:
 * - setMessageReaction REPLACES the message's reaction set, so start and
 *   settle are each a single call — no separate DELETE of the 👀.
 * - Discord subtext (-#) becomes italic HTML (<i>), with blurb content
 *   escaped so reasoning text can never inject entities.
 * - A typing keepalive (sendChatAction) is available for noticeably slow
 *   executions; Discord has no equivalent surface.
 *
 * Callers hand in the rate-limited TelegramApi wrapper so narration shares
 * the per-chat send budget; the narrator itself is transport-agnostic.
 */
export class TelegramNarrator {
  private readonly api: TelegramApi;
  private readonly logger: Logger;
  private readonly clock: TelegramNarratorClock;
  private readonly chatId: number | string;
  private readonly messageThreadId: number | null;
  private readonly triggerMessageId: number;
  private readonly minPostGapMs: number;
  private readonly maxPosts: number;
  private readonly typingIntervalMs: number;
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
  private typingStop: (() => void) | null = null;
  private typingLoop: Promise<void> | null = null;

  private constructor(
    api: TelegramApi,
    target: TelegramNarratorTarget,
    options: TelegramNarratorOptions,
  ) {
    this.api = api;
    this.logger = options.logger;
    this.clock = options.clock ?? {
      now: () => nowMs(),
      sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    };
    this.chatId = target.chatId;
    this.messageThreadId = target.messageThreadId ?? null;
    this.triggerMessageId = target.triggerMessageId;
    this.minPostGapMs = options.minPostGapMs ?? NARRATOR_MIN_POST_GAP_MS;
    this.maxPosts = options.maxPosts ?? NARRATOR_MAX_POSTS;
    this.typingIntervalMs =
      options.typingIntervalMs ?? TYPING_KEEPALIVE_INTERVAL_MS;
  }

  /** Adds the 👀 working reaction (best-effort) and returns the narrator. */
  static start(
    api: TelegramApi,
    target: TelegramNarratorTarget,
    options: TelegramNarratorOptions,
  ): TelegramNarrator {
    const narrator = new TelegramNarrator(api, target, options);
    narrator.enqueueReaction(REACTION_WORKING);
    return narrator;
  }

  /**
   * Server-side activity summaries (renderer.status events) — the Telegram
   * analog of Slack's assistant status. Telegram has no ephemeral status
   * surface, so summaries post as append-only italic blurbs. Empty statuses
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

  update(chunk: TelegramNarratorChunk): void {
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
   * Keeps the chat's "typing…" indicator alive on a ~4s cadence (below the
   * indicator's ~5s lifetime so it never lapses between refreshes). Callers
   * start it only once execution is noticeably slow (an instant answer should
   * not flash a typing bubble) and it stops automatically on finish. Purely
   * cosmetic: sendChatAction failures are logged and the loop keeps going.
   */
  startTypingKeepalive(): void {
    if (this.finished || this.typingStop) return;
    let stop!: () => void;
    const stopped = new Promise<void>((resolve) => {
      stop = resolve;
    });
    this.typingStop = stop;
    this.typingLoop = this.runTypingLoop(stopped);
  }

  stopTypingKeepalive(): void {
    this.typingStop?.();
    this.typingStop = null;
  }

  /**
   * Posts any remaining thought, stops the typing keepalive, then settles the
   * reaction: 👍 on success, 👎 on failure, and 👀 stays put for "retrying"
   * (the retry attempt re-adds it; the set-reaction call is idempotent).
   * Never throws — narration is cosmetic. A "done" outcome downgrades to
   * "failed" when an error task was seen (the renderer surfaces in-stream
   * failures as error tasks, not throws).
   */
  async finish(outcome: TelegramNarratorOutcome): Promise<void> {
    if (this.finished) return;
    this.finished = true;
    this.stopTypingKeepalive();
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.flushPendingText();
    this.enqueueBlurbPost();
    const failed =
      outcome === "failed" || (outcome === "done" && this.sawError);
    if (outcome !== "retrying") {
      // Telegram delta: one call replaces 👀 with the settled emoji — the
      // message always carries exactly one indicator, with no removal step.
      this.enqueueReaction(failed ? REACTION_FAILED : REACTION_DONE);
    }
    await this.chain;
    await this.typingLoop;
    if (this.droppedBlurbs) {
      this.logger.debug("telegrambot_narrator_blurbs_dropped", {
        dropped: this.droppedBlurbs,
      });
    }
  }

  private async runTypingLoop(stopped: Promise<void>): Promise<void> {
    let stopRequested = false;
    void stopped.then(() => {
      stopRequested = true;
    });
    while (!stopRequested && !this.finished) {
      // Cadence is measured from the start of each send: the send may sit
      // behind paced queue work, and sleeping the full interval after it
      // completes would stretch the effective period past the indicator's
      // ~5s lifetime.
      const sendStartedAtMs = this.clock.now();
      try {
        await this.api.sendChatAction({
          chat_id: this.chatId,
          action: "typing",
          ...(this.messageThreadId === null
            ? {}
            : { message_thread_id: this.messageThreadId }),
        });
      } catch (error) {
        this.logger.warn("telegrambot_narrator_typing_failed", {
          chat_id: String(this.chatId),
          error: errorMessage(error),
        });
      }
      if (stopRequested || this.finished) return;
      const elapsedMs = this.clock.now() - sendStartedAtMs;
      await Promise.race([
        this.clock.sleep(Math.max(0, this.typingIntervalMs - elapsedMs)),
        stopped,
      ]);
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
    const params: SendMessageParams = {
      chat_id: this.chatId,
      text: this.renderBlurbs(blurbs),
      parse_mode: "HTML",
      // Reasoning text may quote URLs; progress noise must not unfurl them.
      link_preview_options: { is_disabled: true },
      ...(this.messageThreadId === null
        ? {}
        : { message_thread_id: this.messageThreadId }),
    };
    this.chain = this.chain.then(async () => {
      try {
        await this.api.sendMessage(params);
      } catch (error) {
        this.logger.warn("telegrambot_narrator_post_failed", {
          chat_id: String(this.chatId),
          error: errorMessage(error),
        });
      }
    });
  }

  /**
   * Telegram delta: Discord's per-line -# subtext becomes one italic segment
   * per blurb (<i> spans newlines), with the content HTML-escaped so agent
   * reasoning can never inject entities. The message budget counts visible
   * characters (Telegram's limit applies after entity parsing); blurbs beyond
   * it are dropped, and the one crossing it is truncated.
   */
  private renderBlurbs(blurbs: string[]): string {
    const pieces: string[] = [];
    let used = 0;
    for (const [index, blurb] of blurbs.entries()) {
      const separator = pieces.length ? 2 : 0;
      const budget = NARRATOR_MESSAGE_MAX_CHARS - used - separator;
      if (budget < NARRATOR_MIN_BLURB_CHARS) {
        this.droppedBlurbs += blurbs.length - index;
        break;
      }
      const text =
        blurb.length <= budget
          ? blurb
          : `${sliceSurrogateSafe(blurb, budget - 1).trimEnd()}…`;
      pieces.push(`<i>${escapeHtml(text)}</i>`);
      used += separator + text.length;
    }
    return pieces.join("\n\n");
  }

  private enqueueReaction(emoji: string): void {
    this.chain = this.chain.then(() =>
      setTelegramReaction(
        this.api,
        {
          chatId: this.chatId,
          emoji,
          messageId: this.triggerMessageId,
        },
        this.logger,
      ),
    );
  }
}

/**
 * Best-effort reaction set-and-replace; never throws (reactions are cosmetic).
 * Shared by the narrator and the ingress guards, mirroring
 * reactToDiscordMessage. Reactions target an existing message id and take no
 * topic id — the message already lives in its topic.
 */
export async function setTelegramReaction(
  api: TelegramApi,
  input: { chatId: number | string; emoji: string; messageId: number },
  logger: Logger,
): Promise<void> {
  try {
    await api.setMessageReaction({
      chat_id: input.chatId,
      message_id: input.messageId,
      reaction: [{ type: "emoji", emoji: input.emoji }],
    });
  } catch (error) {
    // Telegram delta: reactions can be structurally unavailable — chats may
    // restrict the allowed reaction set and some messages cannot be reacted
    // to at all. Both surface as 400s naming the reaction; classify them so
    // an expected restriction is distinguishable from a real fault, but
    // suppress either way — cosmetic failures must never fail the run.
    const event = isReactionUnavailable(error)
      ? "telegrambot_narrator_reaction_unavailable"
      : "telegrambot_narrator_reaction_failed";
    logger.warn(event, {
      chat_id: String(input.chatId),
      emoji: input.emoji,
      message_id: input.messageId,
      error: errorMessage(error),
    });
  }
}

function isReactionUnavailable(error: unknown): boolean {
  return (
    error instanceof TelegramApiError &&
    error.status === 400 &&
    /reaction|react/i.test(error.description ?? "")
  );
}

/** Escape for Telegram HTML parse mode (only &, <, > are significant). */
function escapeHtml(text: string): string {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

// Same motivation as the Discord port: truncation cuts are surrogate-safe
// because halving an emoji's surrogate pair yields an invalid payload.
function truncateBlurb(text: string): string {
  if (text.length <= NARRATOR_BLURB_MAX_CHARS) return text;
  return `${sliceSurrogateSafe(text, NARRATOR_BLURB_MAX_CHARS - 1).trimEnd()}…`;
}
