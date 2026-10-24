import { describe, expect, test } from "bun:test";
import {
  executeSessionTurn,
  isRetryableSessionApiError,
} from "../src/session-api";
import type { GithubbotApiMessage, GithubbotOptions } from "../src/types";

const THREAD_ID = "github:owner/repo:1";

const message: GithubbotApiMessage = {
  attachments: [],
  author: {
    fullName: "Test User",
    isBot: false,
    isMe: false,
    userId: "1",
    userName: "test",
  },
  id: "comment-1",
  isMention: true,
  raw: {},
  text: "hi",
  threadId: THREAD_ID,
  timestamp: "2026-06-10T00:00:00.000Z",
};

describe("executeSessionTurn conflicts", () => {
  async function executeError(body: unknown): Promise<unknown> {
    const options: GithubbotOptions = {
      apiUrl: "http://api.test",
      fetch: async () => Response.json(body, { status: 409 }),
      token: "token",
      webhookSecret: "secret",
    };
    try {
      await executeSessionTurn(options, {
        afterEventId: 0,
        executeMessage: message,
        messages: [message],
        onEventId: () => undefined,
        openStream: false,
        threadId: THREAD_ID,
      });
    } catch (error) {
      return error;
    }
    throw new Error("expected execute to fail");
  }

  test("retries an execute rejected because another execution is active", async () => {
    const error = await executeError({
      code: "active_execution_conflict",
      error: "session already has a queued or running execution",
      ok: false,
    });
    expect(isRetryableSessionApiError(error)).toBe(true);
  });

  test("does not retry other execute conflicts", async () => {
    const error = await executeError({ error: "conflict", ok: false });
    expect(isRetryableSessionApiError(error)).toBe(false);
  });
});
