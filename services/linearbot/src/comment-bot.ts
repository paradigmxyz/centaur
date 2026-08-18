import type { ChatSDKStreamChunk } from "@centaur/rendering";

// Centaur-forward Linear model: threads are king. A Linear comment thread maps
// to one centaur sandbox/context, and the bot answers IN the thread with a
// single visible comment that is live-edited: posted with the first thought as a
// collapsed "Thinking…" section, then swapped in place to the answer with its
// chain-of-thought tucked into a collapsed section (slackbot-style). The
// agent-session widget is vestigial (settled minimally; see index.ts). This
// module is the pure rendering side: mention detection, the stream→reply
// collector, and the comment bodies (thinking + final).

const COT_MAX_LINES = 40;
const COT_LINE_MAX_CHARS = 300;
const COT_TOTAL_MAX_CHARS = 8_000;
const ANSWER_MAX_CHARS = 50_000;

type CommentBotTaskChunk = Extract<ChatSDKStreamChunk, { type: "task_update" }>;

/**
 * True when the comment addresses the bot. Linear encodes a mention as the
 * mentioned profile's PLAIN URL in the markdown body
 * (`https://linear.app/{ws}/profiles/{handle}`) — NOT `@name` text or the user
 * UUID (see linear.app/developers/graphql#adding-mentions-in-markdown). So match
 * the bot's profile handle in such a URL first; fall back to the user id and a
 * typed `@name` for robustness.
 */
export function commentMentionsBot(
  body: string,
  names: string[],
  markers: { botUserId?: string; profileHandle?: string } = {},
): boolean {
  if (
    markers.profileHandle &&
    body.includes(`/profiles/${markers.profileHandle}`)
  ) {
    return true;
  }
  if (markers.botUserId && body.includes(markers.botUserId)) return true;
  const haystack = body.toLowerCase();
  return names.some((name) => {
    const needle = name.trim().toLowerCase();
    return needle.length > 0 && haystack.includes(`@${needle}`);
  });
}

/**
 * Accumulates a streamed run into the two parts of a comment reply: the answer
 * markdown, and a chain-of-thought transcript (reasoning + tool actions). Built
 * to mirror the agent-session narrator's selection logic, but flattened into a
 * single collapsed block instead of live activities.
 */
export class CommentReplyCollector {
  private answerText = "";
  private cot: string[] = [];
  private cotChars = 0;
  // The renderer re-emits terminal task updates at stream close; one line per
  // task id is enough (mirrors the narrator's settledTaskIds).
  private readonly recordedTaskIds = new Set<string>();
  // The command/reasoning text arrives on the in-progress update; the terminal
  // update often omits `details` (carrying only output). Cache per task id so
  // the settled line keeps its parameter — mirrors the narrator's taskDetails.
  private readonly taskDetails = new Map<string, string>();
  private sawError = false;
  private errorTextValue = "";
  // The most recent reasoning line — shown live in the comment body (outside the
  // collapsed transcript) as "what I'm doing now".
  private latestThoughtText = "";

  update(chunk: ChatSDKStreamChunk): void {
    if (chunk.type === "markdown_text") {
      this.answerText += chunk.text;
      return;
    }
    if (chunk.type === "plan_update") {
      this.pushCot(`▸ ${chunk.title}`);
      return;
    }
    if (chunk.type !== "task_update") return;
    if (chunk.details) this.taskDetails.set(chunk.id, chunk.details);
    if (chunk.status === "error") {
      this.sawError = true;
      this.errorTextValue = [chunk.title, chunk.output ?? chunk.details]
        .filter(Boolean)
        .join("\n");
    }
    const line = flattenCotLine(this.formatTaskLine(chunk));
    if (chunk.title === "Thinking" && line) {
      this.latestThoughtText = line.slice(0, THOUGHT_MAX_CHARS);
    }
    if (chunk.status !== "complete" && chunk.status !== "error") {
      if (!line || this.recordedTaskIds.has(chunk.id)) return;
      this.recordedTaskIds.add(chunk.id);
      this.pushCot(line);
      return;
    }
    if (this.recordedTaskIds.has(chunk.id)) return;
    this.recordedTaskIds.add(chunk.id);
    this.pushCot(line);
  }

  private formatTaskLine(chunk: CommentBotTaskChunk): string {
    const detail = (
      this.taskDetails.get(chunk.id) ??
      chunk.details ??
      chunk.output ??
      ""
    ).trim();
    // A bare "Thinking" with no captured reasoning is noise; skip it.
    if (chunk.title === "Thinking") return detail;
    return detail ? `${chunk.title}: ${detail}` : chunk.title;
  }

  get answer(): string {
    return this.answerText.trim();
  }

  get cotLines(): string[] {
    return this.cot;
  }

  get failed(): boolean {
    return this.sawError;
  }

  get errorText(): string {
    return this.errorTextValue;
  }

  /** The latest reasoning line, shown live in the comment body. */
  get latestThought(): string {
    return this.latestThoughtText;
  }

  private pushCot(line: string): void {
    const flattened = flattenCotLine(line);
    if (!flattened) return;
    if (
      this.cot.length >= COT_MAX_LINES ||
      this.cotChars >= COT_TOTAL_MAX_CHARS
    )
      return;
    const capped = capCotLine(flattened);
    this.cot.push(capped);
    this.cotChars += capped.length;
  }
}

const INLINE_CODE_MAX_CHARS = 240;
const THOUGHT_MAX_CHARS = 600;

/**
 * Makes a task/reasoning detail safe for a single bullet line: turns fenced code
 * blocks into inline code spans (so a command renders as `the command`, not a
 * ```fence``` that swallows the rest of the list), strips stray fences, and
 * collapses newlines so the entry stays on its own bullet.
 */
function flattenCotLine(text: string): string {
  return text
    .replace(/```[^\n`]*\r?\n?([\s\S]*?)```/g, (_match, code) =>
      inlineCode(String(code)),
    )
    .replace(/`{3,}/g, "")
    .replace(/\r?\n+/g, " ")
    .replace(/[ \t]{2,}/g, " ")
    .trim();
}

/**
 * Wraps text as an inline code span, bounded and stripped of backticks so the
 * single-backtick delimiters can never be left unbalanced by the content.
 */
function inlineCode(code: string): string {
  let inner = code
    .replace(/\r?\n+/g, " ")
    .replace(/`/g, "'")
    .replace(/[ \t]{2,}/g, " ")
    .trim();
  if (!inner) return "";
  if (inner.length > INLINE_CODE_MAX_CHARS) {
    inner = `${inner.slice(0, INLINE_CODE_MAX_CHARS)}…`;
  }
  return `\`${inner}\``;
}

/** Cap a (flattened) cot line, closing a code span the cut may have opened. */
function capCotLine(text: string): string {
  if (text.length <= COT_LINE_MAX_CHARS) return text;
  let capped = `${text.slice(0, COT_LINE_MAX_CHARS)}…`;
  // inlineCode uses single-backtick delimiters with no inner backticks, so an
  // odd backtick count means the cut landed inside a span — close it.
  if ((capped.match(/`/g) ?? []).length % 2 === 1) capped = `${capped}\``;
  return capped;
}

/**
 * The in-progress body for the live reply: the latest reasoning as a live
 * headline in the body, with the full chain-of-thought so far folded into a
 * collapsed "Thinking…" section beneath it. Posted with the first thought and
 * edited as more arrive; once the run settles, buildCommentReplyBody replaces
 * the headline with the answer and relabels the section "Chain of thought".
 */
export function buildThinkingReplyBody(
  cotLines: string[],
  currentThought?: string,
): string {
  const cot = cotLines.length
    ? cotLines.map((line) => `- ${line}`).join("\n")
    : "…";
  const section = collapsibleSection("Thinking…", cot);
  const headline = currentThought?.trim();
  return headline ? `${headline}\n\n${section}` : section;
}

/**
 * Composes the single comment posted back to the thread: the answer, then the
 * chain-of-thought folded into a collapsed section (omitted when empty).
 */
export function buildCommentReplyBody(input: {
  answer: string;
  cotLines: string[];
  fallback?: string;
}): string {
  const raw =
    input.answer.trim() ||
    input.fallback?.trim() ||
    "Execution completed, but no final text was captured.";
  const answer =
    raw.length > ANSWER_MAX_CHARS
      ? `${raw.slice(0, ANSWER_MAX_CHARS).trimEnd()}\n[truncated]`
      : raw;
  if (input.cotLines.length === 0) return answer;
  const cot = input.cotLines.map((line) => `- ${line}`).join("\n");
  return `${answer}\n\n${collapsibleSection("Chain of thought", cot)}`;
}

/**
 * A Linear collapsible section (the editor's `>>>` text shortcut). Isolated so
 * the exact markdown can be tweaked once verified against a live workspace — if
 * Linear does not honor `>>>` in a `commentCreate` body the content still
 * renders, just un-collapsed under a literal `>>>` line.
 */
export function collapsibleSection(summary: string, body: string): string {
  return `>>> ${summary}\n${body}`;
}
