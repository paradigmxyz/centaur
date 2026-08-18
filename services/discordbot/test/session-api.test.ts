import { describe, expect, it } from "bun:test";
import type { Attachment } from "chat";
import {
  codexAttachmentInput,
  forwardToSessionApi,
  isContentlessApiMessage,
  isDiscordPermissionError,
  isRetryableSessionApiError,
  MAX_INLINE_ATTACHMENT_BYTES,
  serializeAttachment,
  SessionApiError,
  toCodexInputLines,
} from "../src/session-api";
import type {
  DiscordbotApiMessage,
  DiscordbotFetch,
  DiscordbotOptions,
  ForwardSessionInput,
} from "../src/types";

type JsonRecord = Record<string, unknown>;

function bytesResponse(body: Buffer): Response {
  return new Response(new Uint8Array(body), { status: 200 });
}

function apiMessage(
  overrides: Partial<DiscordbotApiMessage> = {},
): DiscordbotApiMessage {
  return {
    attachments: [],
    author: {
      fullName: "Alice",
      isBot: false,
      isMe: false,
      userId: "u1",
      userName: "alice",
    },
    id: "m1",
    isMention: true,
    raw: {},
    text: "hello",
    threadId: "discord:G1:C1:T1",
    timestamp: "2026-01-01T00:00:00.000Z",
    ...overrides,
  };
}

describe("isRetryableSessionApiError", () => {
  it("respects the SessionApiError retryable flag", () => {
    const retryable = new SessionApiError({
      action: "create session",
      body: "",
      retryable: true,
      status: 503,
      statusText: "Service Unavailable",
    });
    const fatal = new SessionApiError({
      action: "create session",
      body: "",
      retryable: false,
      status: 400,
      statusText: "Bad Request",
    });
    expect(isRetryableSessionApiError(retryable)).toBe(true);
    expect(isRetryableSessionApiError(fatal)).toBe(false);
  });

  it("treats AbortError as retryable", () => {
    const error = new Error("aborted");
    error.name = "AbortError";
    expect(isRetryableSessionApiError(error)).toBe(true);
  });

  it("treats TypeError as retryable (fetch network failures), relying on the render retry cap to bound programming bugs", () => {
    // Deliberate parity with slackbotv2: WHATWG fetch surfaces network
    // failures as TypeError, so dropping it would lose transient blips. The
    // RENDER_RETRY_MAX_ATTEMPTS cap in index.ts is what prevents a TypeError
    // thrown by a programming bug from looping forever.
    expect(isRetryableSessionApiError(new TypeError("fetch failed"))).toBe(
      true,
    );
  });

  it("does not retry generic errors or non-errors", () => {
    expect(isRetryableSessionApiError(new Error("boom"))).toBe(false);
    expect(isRetryableSessionApiError("boom")).toBe(false);
    expect(isRetryableSessionApiError(undefined)).toBe(false);
  });
});

describe("isDiscordPermissionError", () => {
  it("parses the Discord error code from the JSON body", () => {
    expect(
      isDiscordPermissionError(
        new Error(
          'Discord API error: 403 {"message":"Missing Access","code":50001}',
        ),
      ),
    ).toBe(true);
    expect(
      isDiscordPermissionError(
        new Error(
          'Discord API error: 403 {"message": "Missing Permissions", "code": 50013}',
        ),
      ),
    ).toBe(true);
  });

  it("does not match thread-not-found errors", () => {
    expect(
      isDiscordPermissionError(
        new Error(
          'Discord API error: 404 {"message": "Unknown Channel", "code": 10003}',
        ),
      ),
    ).toBe(false);
    expect(isDiscordPermissionError(new Error("boom"))).toBe(false);
    expect(isDiscordPermissionError("boom")).toBe(false);
  });
});

describe("forwardToSessionApi principal naming", () => {
  function recorderApi(): {
    fetchFn: DiscordbotFetch;
    creates: Array<Record<string, unknown>>;
  } {
    const creates: Array<Record<string, unknown>> = [];
    const fetchFn: DiscordbotFetch = async (input, init) => {
      const url = String(input);
      const body = init?.body ? JSON.parse(String(init.body)) : undefined;
      if (url.endsWith("/execute")) {
        return Response.json({
          execution_id: "exec-1",
          ok: true,
          status: "running",
          thread_key: "discord:G1:C1:T1",
        });
      }
      if (url.endsWith("/messages")) return Response.json({ ok: true });
      creates.push(body);
      return Response.json({ ok: true });
    };
    return { fetchFn, creates };
  }

  function options(fetchFn: DiscordbotFetch): DiscordbotOptions {
    return {
      apiUrl: "http://api.test",
      applicationId: "app",
      botToken: "token",
      fetch: fetchFn,
      publicKey: "key",
    };
  }

  function forwardInput(
    overrides: Partial<ForwardSessionInput> = {},
  ): ForwardSessionInput {
    return {
      afterEventId: 0,
      executeMessage: apiMessage(),
      messages: [apiMessage()],
      onEventId: () => undefined,
      openStream: false,
      threadId: "discord:G1:C1:T1",
      ...overrides,
    };
  }

  it("carries the channel name as create-session metadata", async () => {
    const { fetchFn, creates } = recorderApi();
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput({ conversationName: "general" }),
    );
    expect(
      (creates[0] as { metadata: { discord_conversation_name?: string } })
        .metadata.discord_conversation_name,
    ).toBe("general");
  });

  it("omits the channel name when unset or blank", async () => {
    const { fetchFn, creates } = recorderApi();
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput({ conversationName: "  " }),
    );
    expect(
      "discord_conversation_name" in
        (creates[0] as { metadata: object }).metadata,
    ).toBe(false);
  });
});

describe("isContentlessApiMessage", () => {
  it("is true for empty text with no attachments (sticker/forward/poll)", () => {
    expect(isContentlessApiMessage(apiMessage({ text: "" }))).toBe(true);
    expect(isContentlessApiMessage(apiMessage({ text: "  \n " }))).toBe(true);
  });

  it("is false when there is text", () => {
    expect(isContentlessApiMessage(apiMessage({ text: "do the thing" }))).toBe(
      false,
    );
  });

  it("is false when an attachment is present even without text", () => {
    expect(
      isContentlessApiMessage(
        apiMessage({ text: "", attachments: [{ type: "image" }] }),
      ),
    ).toBe(false);
  });
});

describe("serializeAttachment", () => {
  it("downloads the CDN url and inlines as base64 when the adapter supplies no bytes", async () => {
    const png = Buffer.from([0x89, 0x50, 0x4e, 0x47, 1, 2, 3, 4]);
    let requestedUrl: string | undefined;
    const fetchFn = (async (url: string) => {
      requestedUrl = url;
      return bytesResponse(png);
    }) as unknown as typeof fetch;
    const attachment = {
      type: "image",
      url: "https://cdn.discordapp.com/attachments/1/2/image.png?ex=1&hm=2",
      name: "image.png",
      mimeType: "image/png",
      size: png.length,
    } as Attachment;

    const result = await serializeAttachment(attachment, fetchFn);

    expect(requestedUrl).toBe(attachment.url);
    expect(result.dataBase64).toBe(png.toString("base64"));
    expect(result.fetchError).toBeUndefined();
  });

  it("prefers adapter-provided bytes over a network fetch", async () => {
    const data = Buffer.from("hello world");
    let fetched = false;
    const fetchFn = (async () => {
      fetched = true;
      return new Response("x", { status: 200 });
    }) as unknown as typeof fetch;

    const result = await serializeAttachment(
      { type: "image", mimeType: "image/png", data } as Attachment,
      fetchFn,
    );

    expect(fetched).toBe(false);
    expect(result.dataBase64).toBe(data.toString("base64"));
  });

  it("skips the download and records fetchError when over the size limit", async () => {
    const fetchFn = (async () => {
      throw new Error("should not be called");
    }) as unknown as typeof fetch;

    const result = await serializeAttachment(
      {
        type: "image",
        url: "https://cdn.discordapp.com/x.png",
        size: MAX_INLINE_ATTACHMENT_BYTES + 1,
      } as Attachment,
      fetchFn,
    );

    expect(result.dataBase64).toBeUndefined();
    expect(result.fetchError).toContain("too large");
  });

  it("records fetchError when the download fails", async () => {
    const fetchFn = (async () =>
      new Response("nope", {
        status: 403,
        statusText: "Forbidden",
      })) as unknown as typeof fetch;

    const result = await serializeAttachment(
      {
        type: "image",
        url: "https://cdn.discordapp.com/x.png",
        mimeType: "image/png",
      } as Attachment,
      fetchFn,
    );

    expect(result.dataBase64).toBeUndefined();
    expect(result.fetchError).toContain("403");
  });
});

describe("codexAttachmentInput", () => {
  it("inlines an image with bytes as a data: URL, never a remote url", () => {
    const out = codexAttachmentInput({
      type: "image",
      mimeType: "image/png",
      dataBase64: "QUJD",
      url: "https://cdn.discordapp.com/x.png",
      name: "image.png",
    }) as JsonRecord;
    expect(out.type).toBe("image");
    expect(out.url).toBe("data:image/png;base64,QUJD");
  });

  it("references a staged attachment id instead of inlining", () => {
    const out = codexAttachmentInput(
      { type: "image", mimeType: "image/png", dataBase64: "QUJD" },
      "att-m1-1",
    ) as JsonRecord;
    expect(out).toMatchObject({
      type: "attachment",
      stagedAttachmentId: "att-m1-1",
    });
    expect(out.dataBase64).toBeUndefined();
    expect(out.url).toBeUndefined();
  });

  it("falls back to the raw url only when no bytes are available", () => {
    const out = codexAttachmentInput({
      type: "image",
      url: "https://cdn.discordapp.com/x.png",
    }) as JsonRecord;
    expect(out.url).toBe("https://cdn.discordapp.com/x.png");
  });
});

describe("toCodexInputLines", () => {
  it("inlines a small image in a single user line as a data: URL", () => {
    const message = apiMessage({
      attachments: [
        {
          type: "image",
          mimeType: "image/png",
          dataBase64: "QUJD",
          name: "image.png",
        },
      ],
    });

    const lines = toCodexInputLines(message, message.threadId);

    expect(lines).toHaveLength(1);
    const content = JSON.parse(lines[0]!).message.content as JsonRecord[];
    const image = content.find((part) => part.type === "image");
    expect(image?.url).toBe("data:image/png;base64,QUJD");
  });

  it("stages a large image as chunk lines plus a referencing user line", () => {
    const dataBase64 = Buffer.alloc(700 * 1024, 1).toString("base64");
    const message = apiMessage({
      attachments: [
        {
          type: "image",
          mimeType: "image/png",
          dataBase64,
          name: "image.png",
        },
      ],
    });

    const lines = toCodexInputLines(message, message.threadId);

    expect(lines.length).toBeGreaterThan(1);
    const chunks = lines.slice(0, -1).map((line) => JSON.parse(line));
    expect(chunks.every((c) => c.type === "attachment.chunk")).toBe(true);
    expect(chunks.at(-1).final).toBe(true);
    // The chunks must reassemble to the original base64 payload.
    expect(chunks.map((c) => c.dataBase64).join("")).toBe(dataBase64);

    const lastLine = lines[lines.length - 1]!;
    const content = JSON.parse(lastLine).message.content as JsonRecord[];
    const ref = content.find((part) => part.type === "attachment");
    expect(ref?.stagedAttachmentId).toBe(chunks[0].attachmentId);
    // The huge payload must NOT also be inlined in the user line.
    expect(lastLine.length).toBeLessThan(dataBase64.length);
  });
});
