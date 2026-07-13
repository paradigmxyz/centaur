import type { ChatSDKStreamChunk } from "@centaur/rendering";

// Threads are king: a GitHub PR/issue comment thread maps to one centaur
// sandbox/context, and the bot answers IN the thread with a single comment — the
// final answer, with its chain-of-thought tucked into a collapsed <details>
// section (slackbot-style). This module is the pure rendering side: the
// stream->reply collector and the comment bodies (thinking + final). Mention
// detection lives in the GitHub chat adapter (matches the bot's @username).

const COT_MAX_LINES = 40;
const COT_LINE_MAX_CHARS = 300;
const COT_TOTAL_MAX_CHARS = 8_000;
const ANSWER_MAX_CHARS = 50_000;

type CommentBotTaskChunk = Extract<ChatSDKStreamChunk, { type: "task_update" }>;

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
  private readonly settledTaskIds = new Set<string>();
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
    // Only persist settled tasks; in-progress repeats are noise in a static
    // transcript.
    if (chunk.status !== "complete" && chunk.status !== "error") return;
    if (this.settledTaskIds.has(chunk.id)) return;
    this.settledTaskIds.add(chunk.id);
    const line = flattenCotLine(this.formatTaskLine(chunk));
    if (chunk.title === "Thinking" && line) {
      this.latestThoughtText = line.slice(0, THOUGHT_MAX_CHARS);
    }
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
 * The in-progress body for the live reply: the latest reasoning as a headline,
 * with the chain-of-thought so far folded into a collapsed "Thinking…" section.
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
 * A GitHub-flavored-markdown collapsible section. GitHub renders raw HTML
 * <details>/<summary> in issue and PR comments, so the chain-of-thought folds
 * away by default and expands on click.
 */
export function collapsibleSection(summary: string, body: string): string {
  return `<details>\n<summary>${summary}</summary>\n\n${body}\n</details>`;
}
