import { describe, expect, test } from "bun:test";
import { executeSessionTurn } from "../src/session-api";
import type {
  ForwardSessionInput,
  GithubbotApiMessage,
  GithubbotOptions,
} from "../src/types";

const message: GithubbotApiMessage = {
  attachments: [],
  author: {
    fullName: "Octo Cat",
    isBot: false,
    isMe: false,
    userId: "123",
    userName: "octocat",
  },
  id: "message-1",
  isMention: true,
  raw: {},
  text: "fix the failing test",
  threadId: "github:example/repo:1",
  timestamp: "2026-07-24T00:00:00.000Z",
};

function input(): ForwardSessionInput {
  return {
    afterEventId: 0,
    executeMessage: message,
    messages: [],
    onEventId: () => {},
    openStream: false,
    threadId: message.threadId,
  };
}

async function captureExecuteRequest(
  maxDurationMs?: number,
): Promise<Record<string, unknown>> {
  let requestBody: Record<string, unknown> | undefined;
  const options: GithubbotOptions = {
    apiKey: "test-api-key",
    apiUrl: "http://centaur-api.test",
    fetch: async (_request, init) => {
      requestBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return Response.json({
        execution_id: "execution-1",
        ok: true,
        status: "running",
      });
    },
    maxDurationMs,
    token: "test-github-token",
    webhookSecret: "test-webhook-secret",
  };

  await executeSessionTurn(options, input());
  if (!requestBody) throw new Error("execute request was not captured");
  return requestBody;
}

describe("executeSessionTurn", () => {
  test("allows an autonomous turn to run for 45 minutes by default", async () => {
    const requestBody = await captureExecuteRequest();

    expect(requestBody.max_duration_ms).toBe(2_700_000);
  });

  test("preserves an explicit maximum duration", async () => {
    const requestBody = await captureExecuteRequest(60_000);

    expect(requestBody.max_duration_ms).toBe(60_000);
  });
});
