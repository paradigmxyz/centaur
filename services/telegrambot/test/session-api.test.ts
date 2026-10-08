import { describe, expect, it } from "bun:test";
import type { Attachment } from "chat";
import {
  forwardToSessionApi,
  isRetryableSessionApiError,
  MAX_INLINE_ATTACHMENT_BYTES,
  SessionApiError,
  serializeAttachment,
} from "../src/session-api";
import type { TelegrambotOptions } from "../src/types";
import { takeMessageChunk, truncateWithNotice } from "../src/utils";

describe("serializeAttachment", () => {
  it("inlines fetchData bytes, including ArrayBuffer results", async () => {
    const bytes = new TextEncoder().encode("png-bytes");
    const attachment: Attachment = {
      fetchData: async () => bytes.buffer as unknown as Buffer,
      mimeType: "image/jpeg",
      type: "image",
    };
    const serialized = await serializeAttachment(attachment);
    expect(serialized.dataBase64).toBe(
      Buffer.from("png-bytes").toString("base64"),
    );
    expect(serialized).not.toHaveProperty("url");
  });

  it("refuses oversized files without downloading them", async () => {
    let fetched = false;
    const serialized = await serializeAttachment({
      fetchData: async () => {
        fetched = true;
        return Buffer.alloc(1);
      },
      size: MAX_INLINE_ATTACHMENT_BYTES + 1,
      type: "file",
    });
    expect(fetched).toBe(false);
    expect(serialized.fetchError).toContain("too large");
  });

  it("records download failures instead of throwing", async () => {
    const serialized = await serializeAttachment({
      fetchData: async () => {
        throw new Error("Failed to download Telegram file abc");
      },
      type: "file",
    });
    expect(serialized.fetchError).toBe("Failed to download Telegram file abc");
  });
});

describe("forwardToSessionApi", () => {
  const options = (
    fetchImpl: TelegrambotOptions["fetch"],
  ): TelegrambotOptions => ({
    apiKey: "key",
    apiUrl: "http://api.test",
    botToken: "1:a",
    fetch: fetchImpl,
  });

  it("keeps api-rs error bodies out of user-visible errors", async () => {
    const error = await forwardToSessionApi(
      options(
        async () =>
          new Response("stack trace at internal-host:5432", {
            status: 500,
            statusText: "Internal Server Error",
          }),
      ),
      {
        afterEventId: 0,
        messages: [],
        onEventId: () => undefined,
        threadId: "telegram:42",
      },
    ).catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(SessionApiError);
    expect((error as Error).message).toBe(
      "Centaur session create session failed: 500 Internal Server Error",
    );
    expect(isRetryableSessionApiError(error)).toBe(true);
  });

  it("classifies client errors as permanent", async () => {
    const error = await forwardToSessionApi(
      options(
        async () =>
          new Response("nope", { status: 403, statusText: "Forbidden" }),
      ),
      {
        afterEventId: 0,
        messages: [],
        onEventId: () => undefined,
        threadId: "telegram:42",
      },
    ).catch((caught: unknown) => caught);
    expect(isRetryableSessionApiError(error)).toBe(false);
  });

  it("sends the bearer and Telegram create metadata", async () => {
    const requests: { url: string; init?: RequestInit }[] = [];
    await forwardToSessionApi(
      options(async (url, init) => {
        requests.push({ url: String(url), init });
        return Response.json({ ok: true });
      }),
      {
        afterEventId: 0,
        conversationName: "Ops",
        messages: [],
        onEventId: () => undefined,
        threadId: "telegram:-100:7",
      },
    );
    expect(requests[0]!.url).toBe(
      "http://api.test/api/session/telegram%3A-100%3A7",
    );
    expect(
      (requests[0]!.init!.headers as Record<string, string>).authorization,
    ).toBe("Bearer key");
    expect(JSON.parse(String(requests[0]!.init!.body)).metadata).toEqual({
      platform: "telegram",
      source: "telegrambot",
      telegram_conversation_name: "Ops",
      thread_id: "telegram:-100:7",
    });
  });
});

describe("answer chunking", () => {
  it("splits at whitespace and re-opens split code fences", () => {
    const text = `intro\n\`\`\`ts\n${"const x = 1;\n".repeat(40)}\`\`\``;
    const split = takeMessageChunk(text, 200)!;
    expect(split.chunk.length).toBeLessThanOrEqual(200);
    expect(split.chunk.endsWith("```")).toBe(true);
    expect(split.rest.startsWith("```ts\n")).toBe(true);
    expect(takeMessageChunk("short", 200)).toBeNull();
  });

  it("truncates honestly and never halves a surrogate pair", () => {
    const truncated = truncateWithNotice(`${"😀".repeat(100)}`, 60, "answer");
    expect(truncated.length).toBeLessThanOrEqual(60);
    expect(truncated).toContain("[truncated");
    expect(truncated).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/);
  });
});
