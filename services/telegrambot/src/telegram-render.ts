import { sliceSurrogateSafe } from "./utils";

/**
 * Markdown -> Telegram HTML rendering and parsed-length-aware chunking.
 *
 * Telegram delta: discordbot posts raw Markdown and only keeps ``` fences
 * balanced across 2000-char chunks (`takeDiscordMessageChunk`). Telegram
 * instead parses formatting server-side, enforces its 4096-char limit on the
 * PARSED text (tag characters are free, entity text is not), and rejects the
 * entire sendMessage call on any unbalanced tag. So the renderer emits only
 * the tag vocabulary Telegram documents — <b> <i> <s> <u> <code> <pre>
 * <pre><code class="language-x"> <a href> <blockquote> <tg-spoiler> — with all
 * text content escaped, and the chunker splits on parsed length, closing every
 * open tag at a boundary and re-opening it (language class included) at the
 * top of the next chunk so each chunk parses independently.
 */

/** Telegram counts sendMessage text in parsed UTF-16 code units, max 4096. */
export const TELEGRAM_MAX_MESSAGE_CHARS = 4096;

export function escapeTelegramHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function escapeTelegramAttribute(text: string): string {
  return escapeTelegramHtml(text).replace(/"/g, "&quot;");
}

// ---------------------------------------------------------------------------
// Markdown -> Telegram HTML
// ---------------------------------------------------------------------------

const FENCE_OPEN = /^\s*```(.*)$/;
const FENCE_CLOSE = /^\s*```\s*$/;
const HEADING = /^#{1,6}\s+(.*)$/;
const BLOCKQUOTE_LINE = /^\s*>\s?/;
const LIST_ITEM = /^(\s*)(?:[-*+]|\d{1,9}[.)])\s+(.*)$/;
const TABLE_ROW = /^\s*\|/;
const TABLE_SEPARATOR = /^\s*\|?[\s:\-|]+\|[\s:\-|]*$/;
const CODE_LANGUAGE = /^[A-Za-z0-9_+#.-]+$/;
const LINK = /^\[([^\]\n]*)\]\(([^()\s]+)\)/;
const SAFE_LINK_SCHEME = /^(https?|tg|mailto):/i;
const MAX_INLINE_DEPTH = 6;

/**
 * Translates the harness's GitHub-flavored Markdown subset into Telegram
 * HTML. Telegram has no block semantics beyond <pre>/<blockquote>, so
 * headings become bold lines and list items become "- " prefixed plain lines;
 * tables have no Telegram representation at all and pass through as <pre> so
 * their alignment survives. Anything unsupported or unterminated degrades to
 * escaped plain text — the output must never be invalid HTML, because
 * Telegram rejects the whole message rather than the offending span.
 */
export function renderMarkdownToTelegramHtml(markdown: string): string {
  const lines = markdown.replace(/\r\n/g, "\n").split("\n");
  const blocks: string[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";

    const fence = FENCE_OPEN.exec(line);
    if (fence) {
      const info = (fence[1] ?? "").trim().split(/\s+/)[0] ?? "";
      const language = CODE_LANGUAGE.test(info) ? info : "";
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !FENCE_CLOSE.test(lines[i] ?? "")) {
        code.push(lines[i] ?? "");
        i += 1;
      }
      i += 1; // consume the closing fence (an unterminated fence eats the rest)
      const body = escapeTelegramHtml(code.join("\n"));
      blocks.push(
        language
          ? `<pre><code class="language-${language}">${body}</code></pre>`
          : `<pre>${body}</pre>`,
      );
      continue;
    }

    const separator = lines[i + 1] ?? "";
    if (
      TABLE_ROW.test(line) &&
      TABLE_SEPARATOR.test(separator) &&
      separator.includes("-")
    ) {
      const rows: string[] = [];
      while (i < lines.length && TABLE_ROW.test(lines[i] ?? "")) {
        rows.push(lines[i] ?? "");
        i += 1;
      }
      blocks.push(`<pre>${escapeTelegramHtml(rows.join("\n"))}</pre>`);
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      blocks.push(`<b>${renderInline(heading[1] ?? "")}</b>`);
      i += 1;
      continue;
    }

    if (BLOCKQUOTE_LINE.test(line)) {
      const quoted: string[] = [];
      while (i < lines.length && BLOCKQUOTE_LINE.test(lines[i] ?? "")) {
        quoted.push(renderInline((lines[i] ?? "").replace(BLOCKQUOTE_LINE, "")));
        i += 1;
      }
      blocks.push(`<blockquote>${quoted.join("\n")}</blockquote>`);
      continue;
    }

    const listItem = LIST_ITEM.exec(line);
    if (listItem) {
      blocks.push(`${listItem[1] ?? ""}- ${renderInline(listItem[2] ?? "")}`);
      i += 1;
      continue;
    }

    blocks.push(renderInline(line));
    i += 1;
  }
  return blocks.join("\n");
}

function renderInline(text: string, depth = 0): string {
  // Depth cap: pathological nesting degrades to plain text instead of
  // recursing without bound.
  if (depth > MAX_INLINE_DEPTH) return escapeTelegramHtml(text);
  let out = "";
  let i = 0;

  const wrap = (marker: string, tag: string): boolean => {
    const close = text.indexOf(marker, i + marker.length);
    if (close === -1) return false;
    const inner = text.slice(i + marker.length, close);
    if (!inner.trim()) return false;
    out += `<${tag}>${renderInline(inner, depth + 1)}</${tag}>`;
    i = close + marker.length;
    return true;
  };

  while (i < text.length) {
    const ch = text[i] ?? "";
    if (ch === "`") {
      let run = 1;
      while (text[i + run] === "`") run += 1;
      const marker = "`".repeat(run);
      const close = text.indexOf(marker, i + run);
      if (close !== -1) {
        out += `<code>${escapeTelegramHtml(text.slice(i + run, close))}</code>`;
        i = close + run;
        continue;
      }
      out += escapeTelegramHtml(marker);
      i += run;
      continue;
    }
    if (ch === "[") {
      const link = LINK.exec(text.slice(i));
      const url = link?.[2] ?? "";
      // Scheme allowlist: a javascript:/data: label must not become a live
      // link, so anything else degrades to the literal markdown text.
      if (link && SAFE_LINK_SCHEME.test(url)) {
        const label = renderInline(link[1] ?? "", depth + 1);
        out += `<a href="${escapeTelegramAttribute(url)}">${label}</a>`;
        i += (link[0] ?? "").length;
        continue;
      }
      out += "[";
      i += 1;
      continue;
    }
    if (text.startsWith("**", i) && wrap("**", "b")) continue;
    if (text.startsWith("__", i) && wrap("__", "b")) continue;
    if (text.startsWith("~~", i) && wrap("~~", "s")) continue;
    if (ch === "*" && wrap("*", "i")) continue;
    if (ch === "_") {
      const prev = i > 0 ? (text[i - 1] ?? "") : "";
      // Intra-word underscores (snake_case identifiers) are not emphasis.
      if (!/[A-Za-z0-9_]/.test(prev) && wrap("_", "i")) continue;
    }
    out += escapeTelegramHtml(ch);
    i += 1;
  }
  return out;
}

// ---------------------------------------------------------------------------
// Telegram HTML tokenizer (shared by parsedTextLength and the chunker)
// ---------------------------------------------------------------------------

type HtmlToken =
  | { kind: "open"; name: string; raw: string }
  | { kind: "close"; name: string }
  | { kind: "text"; text: string };

const TAG_TOKEN = /^<(\/?)([a-zA-Z][a-zA-Z0-9-]*)((?:\s[^<>]*)?)(\/?)>/;
const ENTITY_TOKEN =
  /^&(?:#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6})|([a-zA-Z][a-zA-Z0-9]{0,30}));/;
const NAMED_ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: " ",
};

function decodeEntityAt(
  html: string,
  index: number,
): { value: string; length: number } | null {
  const match = ENTITY_TOKEN.exec(html.slice(index));
  if (!match) return null;
  const raw = match[0] ?? "";
  const name = match[3];
  if (name !== undefined) {
    const value = NAMED_ENTITIES[name.toLowerCase()];
    return value === undefined ? null : { value, length: raw.length };
  }
  const codePoint =
    match[1] !== undefined
      ? Number(match[1])
      : Number.parseInt(match[2] ?? "", 16);
  if (!Number.isFinite(codePoint) || codePoint > 0x10ffff) return null;
  // A numeric reference to a lone surrogate would poison every downstream
  // UTF-16 cut; leave the ampersand as literal text instead.
  if (codePoint >= 0xd800 && codePoint <= 0xdfff) return null;
  return { value: String.fromCodePoint(codePoint), length: raw.length };
}

/** Text tokens carry DECODED text so all length math is in parsed units. */
function tokenizeTelegramHtml(html: string): HtmlToken[] {
  const tokens: HtmlToken[] = [];
  let text = "";
  const flushText = () => {
    if (text) {
      tokens.push({ kind: "text", text });
      text = "";
    }
  };
  let i = 0;
  while (i < html.length) {
    const ch = html[i] ?? "";
    if (ch === "<") {
      const match = TAG_TOKEN.exec(html.slice(i));
      if (match) {
        const raw = match[0] ?? "";
        const name = (match[2] ?? "").toLowerCase();
        flushText();
        if (match[1]) tokens.push({ kind: "close", name });
        else if (!match[4]) tokens.push({ kind: "open", name, raw });
        // Self-closing tags carry no formatting state and no text; dropped.
        i += raw.length;
        continue;
      }
      text += "<";
      i += 1;
      continue;
    }
    if (ch === "&") {
      const entity = decodeEntityAt(html, i);
      if (entity) {
        text += entity.value;
        i += entity.length;
        continue;
      }
      text += "&";
      i += 1;
      continue;
    }
    text += ch;
    i += 1;
  }
  flushText();
  return tokens;
}

/**
 * Length of the message as Telegram counts it: UTF-16 code units of the
 * parsed entity text, excluding all tag characters.
 */
export function parsedTextLength(html: string): number {
  let length = 0;
  for (const token of tokenizeTelegramHtml(html)) {
    if (token.kind === "text") length += token.text.length;
  }
  return length;
}

// ---------------------------------------------------------------------------
// Chunker
// ---------------------------------------------------------------------------

type OpenTag = { name: string; raw: string };
type ChunkPosition = { token: number; offset: number };
type ChunkCut = {
  html: string;
  plain: string;
  parsed: number;
  next: ChunkPosition;
  stack: OpenTag[];
};
type ChunkPiece = {
  html: string;
  plain: string;
  next: ChunkPosition;
  stack: OpenTag[];
  done: boolean;
};

function closingTags(stack: readonly OpenTag[]): string {
  return stack
    .map((tag) => `</${tag.name}>`)
    .reverse()
    .join("");
}

const EMPTY_ELEMENT = /<([a-zA-Z][a-zA-Z0-9-]*)(?:\s[^<>]*)?><\/\1>/g;

/**
 * A cut that lands directly after an open tag (or emptied a fence opener)
 * leaves `<pre><code ...></code></pre>` husks behind; they are valid but
 * render as stray blocks, so strip them. The open stack still carries the
 * tags, so the next chunk re-opens them correctly.
 */
function stripEmptyElements(html: string): string {
  let current = html;
  for (;;) {
    const stripped = current.replace(EMPTY_ELEMENT, "");
    if (stripped === current) return current;
    current = stripped;
  }
}

function takeChunk(
  tokens: readonly HtmlToken[],
  start: ChunkPosition,
  startStack: readonly OpenTag[],
  limit: number,
): ChunkPiece {
  // Formatting that spans the previous boundary re-opens verbatim (raw open
  // tag), which is what keeps a language-classed code block continuing
  // seamlessly across chunks.
  let out = startStack.map((tag) => tag.raw).join("");
  let plain = "";
  let parsed = 0;
  const stack: OpenTag[] = startStack.slice();
  let tokenIndex = start.token;
  let offset = start.offset;
  // Mirror of the Discord chunker's boundary preference: only boundaries in
  // the latter half of the window qualify, the latest boundary outside a code
  // block wins over any (even later) newline inside one, and a hard cut is
  // the last resort.
  const minCut = Math.max(1, Math.floor(limit / 2));
  let lastOutsideCode: ChunkCut | null = null;
  let lastInsideCodeNewline: ChunkCut | null = null;

  const cutHere = (next: ChunkPosition): ChunkCut => ({
    html: out,
    plain,
    parsed,
    next,
    stack: stack.slice(),
  });

  while (tokenIndex < tokens.length) {
    const token = tokens[tokenIndex];
    if (!token) break;
    if (token.kind === "open") {
      out += token.raw;
      stack.push({ name: token.name, raw: token.raw });
      tokenIndex += 1;
      offset = 0;
      continue;
    }
    if (token.kind === "close") {
      // Drop a close that does not match the innermost open (malformed
      // input); emitting it would unbalance every downstream chunk.
      if (stack.length > 0 && stack[stack.length - 1]?.name === token.name) {
        stack.pop();
        out += `</${token.name}>`;
      }
      tokenIndex += 1;
      offset = 0;
      continue;
    }

    const text = token.text;
    const insideCode = stack.some(
      (tag) => tag.name === "pre" || tag.name === "code",
    );
    while (offset < text.length) {
      const ch = text[offset] ?? "";
      // Inside <pre>/<code> only newlines are safe boundaries — dropping a
      // space would corrupt code alignment. Outside, any whitespace works.
      const isBoundary =
        ch === "\n" || ((ch === " " || ch === "\t") && !insideCode);
      if (isBoundary && parsed >= minCut) {
        const cut = cutHere({ token: tokenIndex, offset: offset + 1 });
        if (!insideCode) lastOutsideCode = cut;
        else if (ch === "\n") lastInsideCodeNewline = cut;
      }
      // Consume a surrogate pair atomically so no cut can land between the
      // halves; sliceSurrogateSafe backing off to "" identifies a pair start.
      const pair = text.slice(offset, offset + 2);
      const width =
        pair.length === 2 && sliceSurrogateSafe(pair, 1) === "" ? 2 : 1;
      if (parsed + width > limit) {
        let cut =
          lastOutsideCode ??
          lastInsideCodeNewline ??
          cutHere({ token: tokenIndex, offset });
        if (cut.parsed <= 0) {
          // Window smaller than one character (e.g. a surrogate pair with
          // limit 1): force progress over strict limit adherence so the
          // caller's guard loop always terminates.
          const forced = text.slice(offset, offset + width);
          out += escapeTelegramHtml(forced);
          plain += forced;
          parsed += width;
          offset += width;
          cut = cutHere({ token: tokenIndex, offset });
        }
        return {
          html: stripEmptyElements(cut.html + closingTags(cut.stack)),
          plain: cut.plain,
          next: cut.next,
          stack: cut.stack,
          done: false,
        };
      }
      const piece = text.slice(offset, offset + width);
      out += escapeTelegramHtml(piece);
      plain += piece;
      parsed += width;
      offset += width;
    }
    tokenIndex += 1;
    offset = 0;
  }

  return {
    html: stripEmptyElements(out + closingTags(stack)),
    plain,
    next: { token: tokens.length, offset: 0 },
    stack: [],
    done: true,
  };
}

/**
 * Splits Telegram HTML into independently valid chunks whose PARSED text
 * length is at most `maxChars` (Telegram's limit ignores tag characters).
 * Never splits inside a tag or an entity; formatting open at a boundary is
 * closed there and re-opened at the top of the next chunk.
 */
export function chunkTelegramHtml(
  html: string,
  maxChars: number = TELEGRAM_MAX_MESSAGE_CHARS,
): string[] {
  const limit = Math.max(1, Math.floor(maxChars));
  const tokens = tokenizeTelegramHtml(html);
  const chunks: string[] = [];
  let position: ChunkPosition = { token: 0, offset: 0 };
  let stack: OpenTag[] = [];
  // The guard bounds runaway splits if a degenerate input ever stops
  // shrinking (same defense as splitDiscordMessageChunks).
  for (let guard = 0; guard < 10_000; guard += 1) {
    if (position.token >= tokens.length) break;
    const piece = takeChunk(tokens, position, stack, limit);
    if (piece.plain.trim()) chunks.push(piece.html);
    if (piece.done) break;
    if (
      piece.next.token === position.token &&
      piece.next.offset === position.offset
    ) {
      break;
    }
    position = piece.next;
    stack = piece.stack;
  }
  return chunks;
}

/**
 * Fallback for when Telegram rejects rendered HTML with a parse/entity error
 * (`isTelegramParseError`): the raw markdown, HTML-escaped with no tags, so
 * the body stays valid under parse_mode=HTML (the chunks are still meant to
 * be sent with parse_mode=HTML — the entities are escaped for it). Escaping
 * guarantees the resend can never be byte-identical to a malformed HTML body:
 * a body that failed to parse contained a `<` or `&`, and here both are
 * always re-escaped.
 */
export function renderPlainTextFallback(
  markdown: string,
  maxChars: number = TELEGRAM_MAX_MESSAGE_CHARS,
): string[] {
  return chunkTelegramHtml(escapeTelegramHtml(markdown), maxChars);
}
