import { describe, expect, test } from "bun:test";
import { forwardToSessionApi } from "../src/session-api";
import type { GithubbotOptions } from "../src/types";

describe("session creation", () => {
  test("defers implicit harness selection to api-rs", async () => {
    let createBody: Record<string, unknown> | undefined;
    const options: GithubbotOptions = {
      apiUrl: "http://api.test",
      fetch: async (_input, init) => {
        createBody = JSON.parse(String(init?.body));
        return Response.json({ harness_switched: false, ok: true });
      },
      token: "github-token",
      webhookSecret: "webhook-secret",
    };

    await forwardToSessionApi(options, {
      afterEventId: 0,
      messages: [],
      onEventId: () => undefined,
      openStream: false,
      threadId: "github:owner:repo:1",
    });

    expect(createBody).toEqual({
      metadata: {
        source: "githubbot",
        platform: "github",
        thread_id: "github:owner:repo:1",
      },
    });
  });
});
