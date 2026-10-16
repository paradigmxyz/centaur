import type { Logger } from "chat";
import type { JsonObject, TelegrambotOptions, TelegrambotTrace } from "./types";

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
  options: TelegrambotOptions,
  event: string,
  trace?: TelegrambotTrace,
  fields: JsonObject = {},
): void {
  const logger = options.logger ?? noopLogger;
  logger.info(event, {
    ...(trace
      ? {
          elapsed_ms: elapsedMs(trace.startedAtMs),
          message_id: trace.messageId,
          mode: trace.mode,
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

export async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

/** Surrogate-safe prefix: never cuts between a UTF-16 surrogate pair. */
export function sliceSurrogateSafe(value: string, maxUnits: number): string {
  if (maxUnits <= 0) return "";
  if (value.length <= maxUnits) return value;
  const tail = value.charCodeAt(maxUnits - 1);
  const end = tail >= 0xd800 && tail <= 0xdbff ? maxUnits - 1 : maxUnits;
  return value.slice(0, end);
}

// Telegram caps a message at 4096 characters after entity parsing, and the
// adapter truncates longer text with an ellipsis. The answer streamer splits
// long answers across messages with these helpers instead (ported from
// discordbot, which has the same problem at Discord's 2000-char cap).

export type MessageChunkSplit = {
  /** Finalized message content, guaranteed to fit in `maxChars`. */
  chunk: string;
  /** Remaining text for the next message (split code fences re-opened). */
  rest: string;
};

const FENCE_CLOSE = "\n```";
const FENCE_LINE = /^\s*```/;

/**
 * Cuts one postable message off the front of `text`, or returns null when the
 * whole text already fits. Prefers newline/whitespace boundaries in the latter
 * half of the window, avoids splitting inside a code fence when a boundary
 * outside one exists, and closes + re-opens a fence when a split inside it is
 * unavoidable. Hard cuts are surrogate-safe.
 */
export function takeMessageChunk(
  text: string,
  maxChars: number,
): MessageChunkSplit | null {
  if (text.length <= maxChars) return null;

  const candidates = fenceStatesByNewline(text, maxChars);
  const stateAt = (pos: number): FenceState => {
    const prior = candidates.filter((candidate) => candidate.index <= pos);
    return prior[prior.length - 1] ?? { index: -1, inFence: false, opener: "" };
  };
  const minCut = Math.floor(maxChars / 2);

  for (let pos = maxChars; pos >= minCut; pos--) {
    const char = text[pos];
    if (char !== "\n" && char !== " " && char !== "\t") continue;
    if (stateAt(pos).inFence) continue;
    const chunk = text.slice(0, pos).trimEnd();
    if (!chunk) break;
    return { chunk, rest: text.slice(pos + 1) };
  }

  for (let pos = maxChars - FENCE_CLOSE.length; pos >= minCut; pos--) {
    if (text[pos] !== "\n") continue;
    const state = stateAt(pos);
    if (!state.inFence) continue;
    return {
      chunk: `${text.slice(0, pos)}${FENCE_CLOSE}`,
      rest: withReopenedFence(state.opener, text.slice(pos + 1), maxChars),
    };
  }

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

type FenceState = { index: number; inFence: boolean; opener: string };

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
  if (!opener || opener.length + 1 >= Math.floor(maxChars / 4)) return rest;
  return `${opener}\n${rest}`;
}

/** Truncates with an honest "[truncated N chars ...]" suffix, never a silent cut. */
export function truncateWithNotice(
  value: string,
  maxChars: number,
  label: string,
): string {
  if (value.length <= maxChars) return value;
  let omitted = value.length - maxChars;
  while (true) {
    const suffix = `\n[truncated ${omitted} chars from ${label}]`;
    const keep = Math.max(0, maxChars - suffix.length);
    const actualOmitted = value.length - keep;
    if (actualOmitted === omitted) {
      return `${sliceSurrogateSafe(value, keep).trimEnd()}${suffix}`;
    }
    omitted = actualOmitted;
  }
}

/**
 * Single-consumer async queue bridging a producer loop to an AsyncIterable
 * consumer. push() never blocks; end() lets the consumer drain and finish.
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

export function splitEnvList(value: string | undefined): string[] {
  return (value ?? "")
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean);
}
