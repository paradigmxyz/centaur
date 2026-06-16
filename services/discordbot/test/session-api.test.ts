import { describe, expect, it } from "bun:test";
import {
  isContentlessApiMessage,
  isDiscordPermissionError,
  isRetryableSessionApiError,
  SessionApiError,
} from "../src/session-api";
import type { DiscordbotApiMessage } from "../src/types";

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
