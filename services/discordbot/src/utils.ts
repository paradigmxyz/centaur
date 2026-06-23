import type { Logger } from "chat";
import type { DiscordbotOptions, DiscordbotTrace, JsonObject } from "./types";

export const noopLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => noopLogger,
};

export function nowMs(): number {
  return globalThis.performance?.now?.() ?? Date.now();
}

export function elapsedMs(startedAtMs: number): number {
  return Math.max(0, Math.round(nowMs() - startedAtMs));
}

export function traceLog(
  options: DiscordbotOptions,
  event: string,
  trace?: DiscordbotTrace,
  fields: JsonObject = {},
): void {
  const logger = options.logger ?? noopLogger;
  logger.info(event, {
    ...(trace
      ? {
          elapsed_ms: elapsedMs(trace.startedAtMs),
          include_context: trace.includeContext,
          message_id: trace.messageId,
          mode: trace.mode,
          open_stream: trace.openStream,
          thread_id: trace.threadId,
        }
      : {}),
    ...fields,
  });
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

export function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

export function isJsonObject(value: unknown): value is JsonObject {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

export async function* toAsyncIterable<T>(
  source: Iterable<T>,
): AsyncIterable<T> {
  for await (const item of source) {
    yield item;
  }
}

// Discord delta (no slackbotv2 analog): Discord rejects payloads that cut a
// UTF-16 surrogate pair in half (400 Invalid Form Body), so every truncation
// of user-visible text must back off the cut when it lands mid-pair.
/** Surrogate-safe prefix: never cuts between a UTF-16 surrogate pair. */
export function sliceSurrogateSafe(value: string, maxUnits: number): string {
  if (maxUnits <= 0) return "";
  if (value.length <= maxUnits) return value;
  const tail = value.charCodeAt(maxUnits - 1);
  const end = tail >= 0xd800 && tail <= 0xdbff ? maxUnits - 1 : maxUnits;
  return value.slice(0, end);
}

// Discord delta (no slackbotv2 analog): Slack accepts ~40k-char messages, but
// Discord caps content at 2000 chars and the chat adapter silently truncates
// with "...". The answer streamer splits long answers across multiple
// messages with these helpers instead.

export type DiscordChunkSplit = {
  /** Finalized message content, guaranteed to fit in `maxChars`. */
  chunk: string;
  /** Remaining text for the next message (split code fences re-opened). */
  rest: string;
};

const FENCE_CLOSE = "\n```";
const FENCE_LINE = /^\s*```/;

/**
 * Cuts one Discord-postable message off the front of `text`, or returns null
 * when the whole text already fits. Prefers newline/whitespace boundaries in
 * the latter half of the window, avoids splitting inside a code fence when a
 * boundary outside one exists, and closes + re-opens a fence when a split
 * inside it is unavoidable. Hard cuts are surrogate-safe.
 */
export function takeDiscordMessageChunk(
  text: string,
  maxChars: number,
): DiscordChunkSplit | null {
  if (text.length <= maxChars) return null;

  const candidates = fenceStatesByNewline(text, maxChars);
  // Fence state for a cut at `pos`: the state AFTER the last full line the
  // chunk would contain. A newline candidate's own state applies to a cut at
  // that newline (the chunk includes the line the newline terminates).
  const stateAt = (pos: number): FenceState => {
    const prior = candidates.filter((candidate) => candidate.index <= pos);
    return prior[prior.length - 1] ?? { index: -1, inFence: false, opener: "" };
  };
  const minCut = Math.floor(maxChars / 2);

  // Prefer the latest newline/whitespace boundary outside any code fence.
  for (let pos = maxChars; pos >= minCut; pos--) {
    const char = text[pos];
    if (char !== "\n" && char !== " " && char !== "\t") continue;
    if (stateAt(pos).inFence) continue;
    const chunk = text.slice(0, pos).trimEnd();
    if (!chunk) break;
    return { chunk, rest: text.slice(pos + 1) };
  }

  // Next best: a newline inside a fence — close the fence in this message and
  // re-open it at the top of the next one.
  for (let pos = maxChars - FENCE_CLOSE.length; pos >= minCut; pos--) {
    if (text[pos] !== "\n") continue;
    const state = stateAt(pos);
    if (!state.inFence) continue;
    return {
      chunk: `${text.slice(0, pos)}${FENCE_CLOSE}`,
      rest: withReopenedFence(state.opener, text.slice(pos + 1), maxChars),
    };
  }

  // Pathological content (one giant line): hard-cut, surrogate-safe, still
  // honoring fence close/re-open when the cut lands inside one.
  const state = stateAt(maxChars);
  if (state.inFence) {
    const chunk = sliceSurrogateSafe(text, maxChars - FENCE_CLOSE.length);
    return {
      chunk: `${chunk}${FENCE_CLOSE}`,
      rest: withReopenedFence(state.opener, text.slice(chunk.length), maxChars),
    };
  }
  const chunk = sliceSurrogateSafe(text, maxChars);
  return { chunk, rest: text.slice(chunk.length) };
}

/** Splits `text` into Discord-postable chunks, each ≤ `maxChars`. */
export function splitDiscordMessageChunks(
  text: string,
  maxChars: number,
): string[] {
  const chunks: string[] = [];
  let rest = text;
  // The guard bounds runaway splits if a degenerate input ever stops shrinking.
  for (let guard = 0; guard < 10_000; guard++) {
    const split = takeDiscordMessageChunk(rest, maxChars);
    if (!split) break;
    if (split.chunk.trim()) chunks.push(split.chunk);
    if (split.rest.length >= rest.length) break;
    rest = split.rest;
  }
  if (rest.trim()) chunks.push(rest);
  return chunks;
}

type FenceState = { index: number; inFence: boolean; opener: string };

/** Code-fence state after each newline within the first `maxChars` chars. */
function fenceStatesByNewline(text: string, maxChars: number): FenceState[] {
  const states: FenceState[] = [];
  let inFence = false;
  let opener = "";
  let lineStart = 0;
  while (lineStart <= maxChars) {
    const newline = text.indexOf("\n", lineStart);
    if (newline === -1 || newline > maxChars) break;
    const line = text.slice(lineStart, newline);
    if (FENCE_LINE.test(line)) {
      inFence = !inFence;
      opener = inFence ? line.trim() : "";
    }
    states.push({ index: newline, inFence, opener });
    lineStart = newline + 1;
  }
  return states;
}

function withReopenedFence(
  opener: string,
  rest: string,
  maxChars: number,
): string {
  // Skip re-opening when the opener would dominate the next message (keeps
  // pathological tiny windows from looping without consuming input).
  if (!opener || opener.length + 1 >= Math.floor(maxChars / 4)) return rest;
  return `${opener}\n${rest}`;
}

// Discord delta (no slackbotv2 analog): in-memory per-guild cap on
// concurrently executing runs. The pod is a singleton by design, so a
// process-local counter is the source of truth.
export class GuildExecutionLimiter {
  private readonly counts = new Map<string, number>();
  private readonly maxPerGuild: number;

  constructor(maxPerGuild: number) {
    this.maxPerGuild = maxPerGuild;
  }

  /** Returns an idempotent release fn, or null when the guild is at its cap. */
  acquire(guildId: string): (() => void) | null {
    const count = this.counts.get(guildId) ?? 0;
    if (count >= this.maxPerGuild) return null;
    this.counts.set(guildId, count + 1);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      const current = this.counts.get(guildId) ?? 0;
      if (current <= 1) this.counts.delete(guildId);
      else this.counts.set(guildId, current - 1);
    };
  }

  inFlight(guildId: string): number {
    return this.counts.get(guildId) ?? 0;
  }
}

/**
 * Single-consumer async queue bridging a producer loop to an AsyncIterable
 * consumer (e.g. the chat SDK's streaming post). push() never blocks; end()
 * lets the consumer drain the remaining items and finish.
 */
export class AsyncTextQueue implements AsyncIterable<string> {
  private readonly values: string[] = [];
  private done = false;
  private wake: (() => void) | null = null;

  push(value: string): void {
    this.values.push(value);
    this.wake?.();
  }

  end(): void {
    this.done = true;
    this.wake?.();
  }

  async *[Symbol.asyncIterator](): AsyncIterator<string> {
    while (true) {
      const value = this.values.shift();
      if (value !== undefined) {
        yield value;
        continue;
      }
      if (this.done) return;
      await new Promise<void>((resolve) => {
        this.wake = () => {
          this.wake = null;
          resolve();
        };
      });
    }
  }
}
