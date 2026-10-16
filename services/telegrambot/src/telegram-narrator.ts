import type { ChatSDKStreamChunk } from "@centaur/rendering";
import type { Logger, Thread } from "chat";
import { errorMessage } from "./utils";

/** Terminal state the run's reaction settles into. */
export type TelegramRunOutcome = "done" | "failed" | "retrying";

// Telegram only accepts reactions from a fixed emoji set; ✅/❌ are not in it
// (setMessageReaction answers 400 REACTION_INVALID), so success and failure
// settle to 👍/👎. One bot reaction per message: each call replaces the last.
export const REACTION_WORKING = "👀";
export const REACTION_DONE = "👍";
export const REACTION_FAILED = "👎";
/** A triggering message that arrived mid-run was appended as context only. */
export const REACTION_NOTED = "✍";
/** A triggering message carried nothing the agent could act on. */
export const REACTION_CONTENTLESS = "🤔";

/**
 * The Telegram-side run status surface: the triggering message gets an
 * instant 👀 reaction while the agent works and settles to 👍 or 👎.
 *
 * Unlike discordbot's narrator this does not post reasoning or activity
 * blurbs. In a group every blurb is a full message that notifies members and
 * spends the per-chat send budget (about 20 messages a minute), so progress
 * stays in the reaction and the typing indicator.
 */
export class TelegramRunIndicator {
  private readonly thread: Thread;
  private readonly messageId: string;
  private readonly logger: Logger;
  private chain: Promise<void> = Promise.resolve();
  private sawError = false;
  private finished = false;

  private constructor(thread: Thread, messageId: string, logger: Logger) {
    this.thread = thread;
    this.messageId = messageId;
    this.logger = logger;
  }

  /** Sets the 👀 working reaction (best-effort) and returns the indicator. */
  static start(
    thread: Thread,
    messageId: string,
    logger: Logger,
  ): TelegramRunIndicator {
    const indicator = new TelegramRunIndicator(thread, messageId, logger);
    indicator.enqueue(REACTION_WORKING);
    return indicator;
  }

  /** The renderer reports in-stream failures as error tasks, not throws. */
  update(chunk: ChatSDKStreamChunk): void {
    if (chunk.type === "task_update" && chunk.status === "error") {
      this.sawError = true;
    }
  }

  /**
   * Settles the reaction: 👍 on success, 👎 on failure (including a "done"
   * run that surfaced an error task), and 👀 stays for "retrying" because the
   * retry attempt re-sets it. Never throws — the reaction is cosmetic.
   */
  async finish(outcome: TelegramRunOutcome): Promise<void> {
    if (this.finished) return;
    this.finished = true;
    if (outcome !== "retrying") {
      const failed =
        outcome === "failed" || (outcome === "done" && this.sawError);
      this.enqueue(failed ? REACTION_FAILED : REACTION_DONE);
    }
    await this.chain;
  }

  private enqueue(emoji: string): void {
    this.chain = this.chain.then(() =>
      reactToTelegramMessage(this.thread, this.messageId, emoji, this.logger),
    );
  }
}

/** Best-effort reaction; never throws (reactions are cosmetic). */
export async function reactToTelegramMessage(
  thread: Pick<Thread, "adapter" | "id">,
  messageId: string,
  emoji: string,
  logger: Logger,
): Promise<void> {
  try {
    await thread.adapter.addReaction(thread.id, messageId, emoji);
  } catch (error) {
    logger.warn("telegrambot_reaction_failed", {
      emoji,
      error: errorMessage(error),
      message_id: messageId,
      thread_id: thread.id,
    });
  }
}
