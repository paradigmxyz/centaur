import { describe, expect, it } from "bun:test";
import {
  TELEGRAM_MAX_MESSAGE_CHARS,
  chunkTelegramHtml,
  escapeTelegramHtml,
  parsedTextLength,
  renderMarkdownToTelegramHtml,
  renderPlainTextFallback,
} from "../src/telegram-render";

// ---------------------------------------------------------------------------
// Test-side strict validator: simulates Telegram's HTML parser. Every chunk
// the service sends must pass this independently — allowed tags only, strictly
// balanced nesting, no raw < > & outside tags/entities, no lone UTF-16
// surrogates. Returns the parsed (entity-decoded, tag-stripped) text.
// ---------------------------------------------------------------------------

const ALLOWED_TAGS = new Set([
  "b",
  "i",
  "s",
  "u",
  "code",
  "pre",
  "a",
  "blockquote",
  "tg-spoiler",
]);

const TAG = /^<(\/?)([a-zA-Z][a-zA-Z0-9-]*)((?:\s[^<>]*)?)>/;
const ENTITY = /^&(amp|lt|gt|quot|#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6}));/;
const ENTITY_VALUES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
};

function parseTelegramChunk(chunk: string): string {
  for (let u = 0; u < chunk.length; u++) {
    const code = chunk.charCodeAt(u);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = chunk.charCodeAt(u + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        throw new Error(`lone high surrogate at ${u}`);
      }
      u += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      throw new Error(`lone low surrogate at ${u}`);
    }
  }

  const stack: string[] = [];
  let text = "";
  let i = 0;
  while (i < chunk.length) {
    const ch = chunk[i] ?? "";
    if (ch === "<") {
      const match = TAG.exec(chunk.slice(i));
      if (!match) throw new Error(`raw '<' at ${i} in ${JSON.stringify(chunk)}`);
      const name = (match[2] ?? "").toLowerCase();
      if (!ALLOWED_TAGS.has(name)) throw new Error(`unsupported tag <${name}>`);
      if (match[1]) {
        const open = stack.pop();
        if (open !== name) throw new Error(`</${name}> closes <${open ?? "nothing"}>`);
      } else {
        stack.push(name);
      }
      i += (match[0] ?? "").length;
      continue;
    }
    if (ch === ">") throw new Error(`raw '>' at ${i} in ${JSON.stringify(chunk)}`);
    if (ch === "&") {
      const entity = ENTITY.exec(chunk.slice(i));
      if (!entity) throw new Error(`raw '&' at ${i} in ${JSON.stringify(chunk)}`);
      const named = ENTITY_VALUES[entity[1] ?? ""];
      if (named !== undefined) text += named;
      else if (entity[2] !== undefined) text += String.fromCodePoint(Number(entity[2]));
      else text += String.fromCodePoint(Number.parseInt(entity[3] ?? "", 16));
      i += (entity[0] ?? "").length;
      continue;
    }
    text += ch;
    i += 1;
  }
  if (stack.length > 0) throw new Error(`unclosed tags: ${stack.join(", ")}`);
  return text;
}

function assertChunksValid(chunks: string[], limit: number): string[] {
  return chunks.map((chunk) => {
    const text = parseTelegramChunk(chunk);
    expect(text.length).toBe(parsedTextLength(chunk));
    expect(text.length).toBeLessThanOrEqual(limit);
    return text;
  });
}

const stripWs = (value: string) => value.replace(/\s+/g, "");

// ---------------------------------------------------------------------------
// renderMarkdownToTelegramHtml
// ---------------------------------------------------------------------------

describe("renderMarkdownToTelegramHtml", () => {
  it("maps bold, italic, and strikethrough", () => {
    expect(renderMarkdownToTelegramHtml("**bold**")).toBe("<b>bold</b>");
    expect(renderMarkdownToTelegramHtml("__bold__")).toBe("<b>bold</b>");
    expect(renderMarkdownToTelegramHtml("*italic*")).toBe("<i>italic</i>");
    expect(renderMarkdownToTelegramHtml("_italic_")).toBe("<i>italic</i>");
    expect(renderMarkdownToTelegramHtml("~~gone~~")).toBe("<s>gone</s>");
  });

  it("nests inline formatting", () => {
    expect(renderMarkdownToTelegramHtml("**bold _and italic_**")).toBe(
      "<b>bold <i>and italic</i></b>",
    );
    // The ambiguous ***-overlap form degrades to valid HTML with literal
    // markers instead of guessing at nesting.
    parseTelegramChunk(renderMarkdownToTelegramHtml("**bold *and italic***"));
  });

  it("does not treat intra-word underscores as emphasis", () => {
    expect(renderMarkdownToTelegramHtml("snake_case_name")).toBe(
      "snake_case_name",
    );
  });

  it("escapes &, <, > in plain text", () => {
    expect(renderMarkdownToTelegramHtml("a & b < c > d")).toBe(
      "a &amp; b &lt; c &gt; d",
    );
  });

  it("maps inline code and escapes its content literally", () => {
    expect(renderMarkdownToTelegramHtml("run `a<b> && c`")).toBe(
      "run <code>a&lt;b&gt; &amp;&amp; c</code>",
    );
    // Markdown markers inside inline code stay literal.
    expect(renderMarkdownToTelegramHtml("`**not bold**`")).toBe(
      "<code>**not bold**</code>",
    );
  });

  it("maps fenced code with a language class", () => {
    const html = renderMarkdownToTelegramHtml(
      '```ts\nconst ok = 1 < 2 && "a" > "b";\n```',
    );
    expect(html).toBe(
      '<pre><code class="language-ts">const ok = 1 &lt; 2 &amp;&amp; "a" &gt; "b";</code></pre>',
    );
  });

  it("maps fenced code without a language to bare <pre>", () => {
    expect(renderMarkdownToTelegramHtml("```\nplain\n```")).toBe(
      "<pre>plain</pre>",
    );
  });

  it("keeps an unterminated fence valid by consuming the rest", () => {
    const html = renderMarkdownToTelegramHtml("```js\nlet x = 1;\nlet y = 2;");
    expect(html).toBe(
      '<pre><code class="language-js">let x = 1;\nlet y = 2;</code></pre>',
    );
    parseTelegramChunk(html);
  });

  it("maps links and escapes the href", () => {
    expect(
      renderMarkdownToTelegramHtml("[docs](https://example.com/?a=1&b=2)"),
    ).toBe('<a href="https://example.com/?a=1&amp;b=2">docs</a>');
  });

  it("degrades unsafe link schemes to literal text", () => {
    const html = renderMarkdownToTelegramHtml("[x](javascript:alert(1))");
    expect(html).not.toContain("<a");
    expect(parseTelegramChunk(html)).toBe("[x](javascript:alert(1))");
  });

  it("renders headings as bold lines", () => {
    expect(renderMarkdownToTelegramHtml("## Heading **two**")).toBe(
      "<b>Heading <b>two</b></b>",
    );
  });

  it("renders list items as '- ' prefixed plain lines", () => {
    const html = renderMarkdownToTelegramHtml(
      ["- one", "* two", "3. three", "4) four"].join("\n"),
    );
    expect(html).toBe(["- one", "- two", "- three", "- four"].join("\n"));
  });

  it("groups consecutive quoted lines into one blockquote", () => {
    expect(renderMarkdownToTelegramHtml("> first & second\n> **third**")).toBe(
      "<blockquote>first &amp; second\n<b>third</b></blockquote>",
    );
  });

  it("passes tables through as <pre> blocks", () => {
    const html = renderMarkdownToTelegramHtml(
      ["| a | b |", "|---|---|", "| 1 | 2 |"].join("\n"),
    );
    expect(html).toBe("<pre>| a | b |\n|---|---|\n| 1 | 2 |</pre>");
  });

  it("degrades unmatched markers to plain text, never invalid HTML", () => {
    for (const input of ["**open", "~~open", "`open", "*", "[label](", "____"]) {
      parseTelegramChunk(renderMarkdownToTelegramHtml(input));
    }
    expect(renderMarkdownToTelegramHtml("**open")).toBe("**open");
  });

  it("returns empty output for empty input", () => {
    expect(renderMarkdownToTelegramHtml("")).toBe("");
  });

  it("produces one independently parseable document for mixed content", () => {
    const md = [
      "# Report",
      "Text with **bold**, `code<>`, and [a link](https://example.com).",
      "> a quote",
      "- a list item",
      "```py",
      "print('x < y')",
      "```",
    ].join("\n");
    const html = renderMarkdownToTelegramHtml(md);
    const text = parseTelegramChunk(html);
    expect(text).toContain("code<>");
    expect(text).toContain("print('x < y')");
  });
});

// ---------------------------------------------------------------------------
// parsedTextLength
// ---------------------------------------------------------------------------

describe("parsedTextLength", () => {
  it("excludes tag characters and counts entities as one char", () => {
    expect(parsedTextLength("<b>ab</b>")).toBe(2);
    expect(parsedTextLength("<b>&amp;</b>")).toBe(1);
    expect(parsedTextLength('<a href="https://example.com/very/long">x</a>')).toBe(1);
    expect(parsedTextLength("")).toBe(0);
    expect(parsedTextLength("plain")).toBe(5);
  });

  it("counts UTF-16 code units like Telegram does", () => {
    expect(parsedTextLength("💥")).toBe(2);
    expect(parsedTextLength("<i>💥</i>")).toBe(2);
  });
});

// ---------------------------------------------------------------------------
// chunkTelegramHtml
// ---------------------------------------------------------------------------

describe("chunkTelegramHtml", () => {
  it("returns a single chunk when the parsed text fits", () => {
    const html = "<b>short</b> message";
    expect(chunkTelegramHtml(html, 100)).toEqual([html]);
  });

  it("returns no chunks for empty or whitespace-only input", () => {
    expect(chunkTelegramHtml("", 100)).toEqual([]);
    expect(chunkTelegramHtml("   \n \t ", 100)).toEqual([]);
  });

  it("uses the 4096-char Telegram limit by default", () => {
    const html = "word ".repeat(1200).trim(); // 5999 parsed chars
    const chunks = chunkTelegramHtml(html);
    expect(chunks.length).toBe(2);
    assertChunksValid(chunks, TELEGRAM_MAX_MESSAGE_CHARS);
  });

  it("splits prose at newline boundaries into independently valid chunks", () => {
    const md = Array.from(
      { length: 30 },
      (_, i) => `paragraph ${i} with **bold** & \`code<>\` ${"word ".repeat(10)}`,
    ).join("\n");
    const html = renderMarkdownToTelegramHtml(md);
    const chunks = chunkTelegramHtml(html, 200);
    expect(chunks.length).toBeGreaterThan(1);
    const texts = assertChunksValid(chunks, 200);
    // Only boundary whitespace may be dropped between chunks.
    expect(stripWs(texts.join(""))).toBe(stripWs(parseTelegramChunk(html)));
  });

  it("enforces the limit on parsed text, not raw HTML length", () => {
    const html = renderMarkdownToTelegramHtml("**ab** ".repeat(80).trim());
    const parsed = parsedTextLength(html); // 239 parsed chars
    expect(html.length).toBeGreaterThan(400);
    const chunks = chunkTelegramHtml(html, 400);
    expect(chunks.length).toBe(1);
    expect(parsedTextLength(chunks[0] ?? "")).toBe(parsed);
  });

  it("never splits inside a tag even when tags straddle the window", () => {
    const html = Array.from(
      { length: 40 },
      (_, i) => `<a href="https://example.com/${i}">${"y".repeat(23)}</a> `,
    ).join("");
    const chunks = chunkTelegramHtml(html, 50);
    expect(chunks.length).toBeGreaterThan(1);
    assertChunksValid(chunks, 50);
  });

  it("closes open formatting at a boundary and re-opens it in the next chunk", () => {
    const html = `<b><i>${"word ".repeat(60).trim()}</i></b>`;
    const chunks = chunkTelegramHtml(html, 100);
    expect(chunks.length).toBeGreaterThan(1);
    assertChunksValid(chunks, 100);
    for (const chunk of chunks) {
      expect(chunk.startsWith("<b><i>")).toBe(true);
      expect(chunk.endsWith("</i></b>")).toBe(true);
    }
  });

  it("continues code fences across chunks with the language class preserved", () => {
    const lines = Array.from({ length: 60 }, (_, i) => `line_${i} = ${i}`);
    const html = renderMarkdownToTelegramHtml(
      `\`\`\`py\n${lines.join("\n")}\n\`\`\``,
    );
    const chunks = chunkTelegramHtml(html, 120);
    expect(chunks.length).toBeGreaterThan(1);
    const texts = assertChunksValid(chunks, 120);
    for (const chunk of chunks) {
      expect(chunk.startsWith('<pre><code class="language-py">')).toBe(true);
      expect(chunk.endsWith("</code></pre>")).toBe(true);
      // No empty husk elements left behind by a cut at a tag edge.
      expect(chunk).not.toMatch(/<([a-zA-Z][a-zA-Z0-9-]*)(?:\s[^<>]*)?><\/\1>/);
    }
    // Splits land on newlines inside the fence: no code line is cut in half.
    const allLines = texts.flatMap((text) => text.split("\n"));
    for (const line of allLines) expect(lines).toContain(line);
    expect(stripWs(texts.join(""))).toBe(stripWs(lines.join("")));
  });

  it("prefers a boundary outside a code block over a later one inside", () => {
    const prose = "p".repeat(50);
    const html = renderMarkdownToTelegramHtml(
      `${prose}\n\`\`\`ts\n${"code line\n".repeat(20)}\`\`\``,
    );
    const chunks = chunkTelegramHtml(html, 80);
    expect(chunks[0]).toBe(prose);
    expect(chunks[1]?.startsWith('<pre><code class="language-ts">')).toBe(true);
    assertChunksValid(chunks, 80);
  });

  it("hard-cuts a single giant line and terminates", () => {
    const html = "x".repeat(1000);
    const chunks = chunkTelegramHtml(html, 100);
    expect(chunks.length).toBe(10);
    const texts = assertChunksValid(chunks, 100);
    expect(texts.join("")).toBe(html);
  });

  it("hard-cuts a giant line inside a code block, keeping <pre> balanced", () => {
    const html = renderMarkdownToTelegramHtml(`\`\`\`\n${"z".repeat(500)}\n\`\`\``);
    const chunks = chunkTelegramHtml(html, 100);
    expect(chunks.length).toBeGreaterThan(1);
    const texts = assertChunksValid(chunks, 100);
    for (const chunk of chunks) {
      expect(chunk.startsWith("<pre>")).toBe(true);
      expect(chunk.endsWith("</pre>")).toBe(true);
    }
    expect(texts.join("")).toBe("z".repeat(500));
  });

  it("never cuts between surrogate pair halves", () => {
    const html = renderMarkdownToTelegramHtml("💥".repeat(300));
    const chunks = chunkTelegramHtml(html, 99);
    expect(chunks.length).toBeGreaterThan(1);
    const texts = assertChunksValid(chunks, 99);
    expect(texts.join("")).toBe("💥".repeat(300));
    for (const text of texts) expect(text.length % 2).toBe(0);
  });

  it("keeps surrogate pairs intact inside a chunked code block", () => {
    const html = renderMarkdownToTelegramHtml(
      `\`\`\`\n${"💥".repeat(200)}\n\`\`\``,
    );
    const texts = assertChunksValid(chunkTelegramHtml(html, 77), 77);
    expect(texts.join("")).toBe("💥".repeat(200));
  });

  it("makes forced progress when the window is smaller than one character", () => {
    // A surrogate pair cannot fit in a 1-unit window; the chunker must still
    // terminate and preserve the content rather than loop.
    const texts = chunkTelegramHtml("💥💥", 1).map(parseTelegramChunk);
    expect(texts.join("")).toBe("💥💥");
  });

  it("drops mismatched close tags instead of emitting invalid chunks", () => {
    const chunks = chunkTelegramHtml("<b>bold</i></b> tail", 100);
    assertChunksValid(chunks, 100);
    expect(chunks[0]).toBe("<b>bold</b> tail");
  });

  it("closes tags left open by malformed input", () => {
    const chunks = chunkTelegramHtml("<b>never closed", 100);
    expect(chunks[0]).toBe("<b>never closed</b>");
    assertChunksValid(chunks, 100);
  });
});

// ---------------------------------------------------------------------------
// renderPlainTextFallback
// ---------------------------------------------------------------------------

describe("renderPlainTextFallback", () => {
  it("emits escaped tag-free chunks that can never repeat a malformed body", () => {
    const malformed = "<div>unsupported & <b>unbalanced</div>";
    const chunks = renderPlainTextFallback(malformed, 100);
    expect(chunks.length).toBe(1);
    const text = parseTelegramChunk(chunks[0] ?? "");
    expect(text).toBe(malformed);
    // No tags survive, and the body differs from the rejected input.
    expect(chunks[0]).not.toContain("<");
    expect(chunks[0]).not.toBe(malformed);
  });

  it("chunks long plain text within the parsed-length limit", () => {
    const markdown = Array.from(
      { length: 40 },
      (_, i) => `row ${i}: value < ${i} & value > ${i - 1}`,
    ).join("\n");
    const chunks = renderPlainTextFallback(markdown, 120);
    expect(chunks.length).toBeGreaterThan(1);
    const texts = assertChunksValid(chunks, 120);
    for (const chunk of chunks) expect(chunk).not.toContain("<");
    expect(stripWs(texts.join(""))).toBe(stripWs(markdown));
  });

  it("applies the 4096 default limit", () => {
    const chunks = renderPlainTextFallback("word ".repeat(1200));
    expect(chunks.length).toBe(2);
    assertChunksValid(chunks, TELEGRAM_MAX_MESSAGE_CHARS);
  });

  it("returns no chunks for empty or whitespace-only input", () => {
    expect(renderPlainTextFallback("", 100)).toEqual([]);
    expect(renderPlainTextFallback("  \n \t ", 100)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// escapeTelegramHtml
// ---------------------------------------------------------------------------

describe("escapeTelegramHtml", () => {
  it("escapes every &, <, and >", () => {
    expect(escapeTelegramHtml("&&<<>>")).toBe(
      "&amp;&amp;&lt;&lt;&gt;&gt;",
    );
    expect(escapeTelegramHtml("plain")).toBe("plain");
  });
});
