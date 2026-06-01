import { afterEach, describe, expect, it, mock } from "bun:test";
import { pollFinalDeliveriesOnce } from "./final-delivery";
import type { AppConfig } from "../config";

const config: AppConfig = {
  NODE_ENV: "test",
  PORT: 3001,
  CENTAUR_API_URL: "http://centaur-api.test",
  CENTAUR_API_KEY: "centaur-test-key",
  CENTAUR_SLACK_EVENTS_PATH: "/api/webhooks/slack",
  RUNTIME_ERROR_ALERT_CHANNEL: "",
  SLACK_EVENT_DEDUP_TTL_MS: 600000,
  SLACK_SIGNATURE_MAX_AGE_SECONDS: 300,
  SLACK_FEEDBACK_COMMANDS: ["/website-feedback"],
  SLACK_FEEDBACK_LINEAR_TEAM_ID: "team-test",
  SLACK_FEEDBACK_LINEAR_PROJECT_ID: "project-test",
  SLACK_FEEDBACK_ALLOWED_CHANNELS: [],
  SLACKBOT_EXTERNAL_ORG_ALLOWLIST: [],
  SLACKBOT_TRIGGER_BOT_ALLOWLIST: [],
};

afterEach(() => {
  mock.restore();
});

describe("final delivery polling", () => {
  it("posts a claimed delivery once and marks it delivered before the next poll", async () => {
    const originalFetch = globalThis.fetch;
    const fetchCalls: Array<{
      path: string;
      body: unknown;
      headers: Record<string, string>;
    }> = [];
    let claimCount = 0;
    const fetchMock = mock(
      async (input: string | URL | Request, init?: RequestInit) => {
        const url = new URL(input instanceof Request ? input.url : input);
        const body = init?.body ? JSON.parse(init.body as string) : undefined;
        fetchCalls.push({
          path: url.pathname,
          body,
          headers: init?.headers as Record<string, string>,
        });

        if (url.pathname === "/agent/final-deliveries/claim") {
          claimCount += 1;
          return jsonResponse({
            deliveries:
              claimCount === 1
                ? [
                    {
                      execution_id: "exe-duplicate-guard",
                      thread_key: "slack:T123:C123:1778883099.579529",
                      trace_id: "90f14ffa-682c-d49f-10a4-efe83a04253d",
                      traceparent:
                        "00-90f14ffa682cd49f10a4efe83a04253d-2cecc7acd547b23b-01",
                      delivery: {
                        platform: "slack",
                        channel: "C123",
                        thread_ts: "1778883099.579529",
                        recipient_team_id: "T123",
                        recipient_user_id: "U123",
                      },
                      final_payload: {
                        session_title: "Centaur · codex",
                        result_text:
                          "done [once](https://example.com) with **bold** text",
                      },
                    },
                  ]
                : [],
          });
        }

        if (
          url.pathname ===
          "/agent/final-deliveries/exe-duplicate-guard/delivered"
        ) {
          return jsonResponse({ ok: true });
        }

        throw new Error(`unexpected request: ${url.pathname}`);
      },
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const slackCalls: Array<{ method: string; params: unknown }> = [];
    const client = {
      assistant: {
        threads: {
          setStatus: async (params: unknown) => {
            slackCalls.push({ method: "assistant.threads.setStatus", params });
            return { ok: true };
          },
        },
      },
      chat: {
        startStream: async (params: any) => {
          slackCalls.push({ method: "chat.startStream", params });
          return { ok: true, channel: params.channel, ts: "1778883100.000000" };
        },
        appendStream: async (params: unknown) => {
          slackCalls.push({ method: "chat.appendStream", params });
          return { ok: true };
        },
        postMessage: async (params: unknown) => {
          slackCalls.push({ method: "chat.postMessage", params });
          return { ok: true };
        },
        stopStream: async (params: unknown) => {
          slackCalls.push({ method: "chat.stopStream", params });
          return { ok: true };
        },
        update: async (params: unknown) => {
          slackCalls.push({ method: "chat.update", params });
          return { ok: true };
        },
      },
      conversations: {
        replies: async (params: unknown) => {
          slackCalls.push({ method: "conversations.replies", params });
          return { ok: true, messages: [] };
        },
      },
    };

    try {
      await pollFinalDeliveriesOnce(config, client as any);
      await pollFinalDeliveriesOnce(config, client as any);

      expect(
        fetchCalls.filter(
          (call) => call.path === "/agent/final-deliveries/claim",
        ),
      ).toHaveLength(2);
      expect(
        fetchCalls.filter(
          (call) =>
            call.path ===
            "/agent/final-deliveries/exe-duplicate-guard/delivered",
        ),
      ).toHaveLength(1);
      const deliveredCall = fetchCalls.find((call) =>
        call.path.endsWith("/delivered"),
      );
      expect(deliveredCall?.headers).toMatchObject({
        "X-Trace-Id": "90f14ffa-682c-d49f-10a4-efe83a04253d",
        "X-Centaur-Thread-Key": "slack:T123:C123:1778883099.579529",
      });
      expect(deliveredCall?.headers.traceparent).toStartWith(
        "00-90f14ffa682cd49f10a4efe83a04253d-",
      );
      expect(
        slackCalls.filter((call) => call.method === "chat.startStream"),
      ).toHaveLength(0);
      expect(
        slackCalls.filter((call) => call.method === "chat.stopStream"),
      ).toHaveLength(0);
      const postMessage = slackCalls.find(
        (call) => call.method === "chat.postMessage",
      );
      expect((postMessage?.params as any)?.text).toBe(
        "done [once](https://example.com) with **bold** text",
      );
      expect((postMessage?.params as any)?.blocks).toEqual([
        {
          type: "markdown",
          text: "done [once](https://example.com) with **bold** text",
        },
      ]);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("does not repost already-delivered fallback chunks after a retry", async () => {
    const originalFetch = globalThis.fetch;
    const fetchCalls: Array<{ path: string; body: any }> = [];
    const posted: any[] = [];
    let claimCount = 0;
    let postCount = 0;
    const delivery = {
      execution_id: "exe-chunk-retry",
      thread_key: "slack:T123:C123:1778883099.579529",
      delivery: {
        platform: "slack",
        channel: "C123",
        thread_ts: "1778883099.579529",
      },
      final_payload: { result_text: `${"a".repeat(4100)} ${"b".repeat(100)}` },
    };
    const fetchMock = mock(
      async (input: string | URL | Request, init?: RequestInit) => {
        const url = new URL(input instanceof Request ? input.url : input);
        const body = init?.body ? JSON.parse(init.body as string) : undefined;
        fetchCalls.push({ path: url.pathname, body });
        if (url.pathname === "/agent/final-deliveries/claim") {
          claimCount += 1;
          return jsonResponse({
            deliveries: claimCount <= 2 ? [delivery] : [],
          });
        }
        if (url.pathname === "/agent/final-deliveries/exe-chunk-retry/failed")
          return jsonResponse({ ok: true });
        if (
          url.pathname === "/agent/final-deliveries/exe-chunk-retry/delivered"
        )
          return jsonResponse({ ok: true });
        throw new Error(`unexpected request: ${url.pathname}`);
      },
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const client = {
      chat: {
        postMessage: async (params: any) => {
          posted.push(params);
          postCount += 1;
          if (postCount === 2)
            return { ok: false, error: "service_unavailable" };
          return { ok: true };
        },
      },
      conversations: {
        replies: async () => ({
          ok: true,
          messages:
            claimCount <= 1
              ? []
              : [
                  {
                    metadata: {
                      event_type: "centaur_final_delivery_chunk",
                      event_payload: {
                        execution_id: "exe-chunk-retry",
                        chunk_index: 0,
                        chunk_count: 2,
                      },
                    },
                  },
                ],
        }),
      },
    };

    try {
      await pollFinalDeliveriesOnce(config, client as any);
      await pollFinalDeliveriesOnce(config, client as any);

      expect(fetchCalls.some((call) => call.path.endsWith("/failed"))).toBe(
        true,
      );
      expect(fetchCalls.some((call) => call.path.endsWith("/delivered"))).toBe(
        true,
      );
      expect(posted).toHaveLength(3);
      expect(posted[0]?.metadata?.event_payload?.chunk_index).toBe(0);
      expect(posted[1]?.metadata?.event_payload?.chunk_index).toBe(1);
      expect(posted[2]?.metadata?.event_payload?.chunk_index).toBe(1);
      expect(
        posted.filter(
          (message) => message.metadata?.event_payload?.chunk_index === 0,
        ),
      ).toHaveLength(1);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("marks permanent Slack post errors non-retryable", async () => {
    const originalFetch = globalThis.fetch;
    const fetchCalls: Array<{ path: string; body: any }> = [];
    const fetchMock = mock(
      async (input: string | URL | Request, init?: RequestInit) => {
        const url = new URL(input instanceof Request ? input.url : input);
        const body = init?.body ? JSON.parse(init.body as string) : undefined;
        fetchCalls.push({ path: url.pathname, body });
        if (url.pathname === "/agent/final-deliveries/claim") {
          return jsonResponse({
            deliveries: [
              {
                execution_id: "exe-channel-missing",
                thread_key: "slack:T123:C123:1778883099.579529",
                delivery: {
                  platform: "slack",
                  channel: "C123",
                  thread_ts: "1778883099.579529",
                },
                final_payload: { result_text: "hello" },
              },
            ],
          });
        }
        if (
          url.pathname === "/agent/final-deliveries/exe-channel-missing/failed"
        )
          return jsonResponse({ ok: true });
        throw new Error(`unexpected request: ${url.pathname}`);
      },
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const client = {
      chat: {
        postMessage: async () => ({ ok: false, error: "channel_not_found" }),
      },
      conversations: { replies: async () => ({ ok: true, messages: [] }) },
    };

    try {
      await pollFinalDeliveriesOnce(config, client as any);
      const failed = fetchCalls.find((call) =>
        call.path.endsWith("/final-deliveries/exe-channel-missing/failed"),
      );
      expect(failed?.body).toMatchObject({
        error_class: "channel_not_found",
        non_retryable: true,
      });
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("marks thrown Slack Web API block-size errors non-retryable", async () => {
    const originalFetch = globalThis.fetch;
    const fetchCalls: Array<{ path: string; body: any }> = [];
    const fetchMock = mock(
      async (input: string | URL | Request, init?: RequestInit) => {
        const url = new URL(input instanceof Request ? input.url : input);
        const body = init?.body ? JSON.parse(init.body as string) : undefined;
        fetchCalls.push({ path: url.pathname, body });
        if (url.pathname === "/agent/final-deliveries/claim") {
          return jsonResponse({
            deliveries: [
              {
                execution_id: "exe-blocks-too-long",
                thread_key: "slack:T123:C123:1778883099.579529",
                delivery: {
                  platform: "slack",
                  channel: "C123",
                  thread_ts: "1778883099.579529",
                },
                final_payload: { result_text: "hello" },
              },
            ],
          });
        }
        if (
          url.pathname ===
          "/agent/final-deliveries/exe-blocks-too-long/failed"
        )
          return jsonResponse({ ok: true });
        throw new Error(`unexpected request: ${url.pathname}`);
      },
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const client = {
      chat: {
        postMessage: async () => {
          const error = new Error("An API error occurred");
          (error as any).data = { ok: false, error: "msg_blocks_too_long" };
          throw error;
        },
      },
      conversations: { replies: async () => ({ ok: true, messages: [] }) },
    };

    try {
      await pollFinalDeliveriesOnce(config, client as any);
      const failed = fetchCalls.find((call) =>
        call.path.endsWith("/final-deliveries/exe-blocks-too-long/failed"),
      );
      expect(failed?.body).toMatchObject({
        error: "An API error occurred",
        error_class: "msg_blocks_too_long",
        non_retryable: true,
      });
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("posts only the missing suffix when live delivery was cut off", async () => {
    const originalFetch = globalThis.fetch;
    const resultText =
      "Already visible in Slack.\n\nThis is the report section that was cut off.";
    const streamedPrefix = "Already visible in Slack.\n\n";
    const fetchCalls: Array<{ path: string; body: unknown }> = [];
    const fetchMock = mock(
      async (input: string | URL | Request, init?: RequestInit) => {
        const url = new URL(input instanceof Request ? input.url : input);
        const body = init?.body ? JSON.parse(init.body as string) : undefined;
        fetchCalls.push({ path: url.pathname, body });
        if (url.pathname === "/agent/final-deliveries/claim") {
          return jsonResponse({
            deliveries: [
              {
                execution_id: "exe-cutoff",
                thread_key: "slack:T123:C123:1778883099.579529",
                delivery: {
                  platform: "slack",
                  channel: "C123",
                  thread_ts: "1778883099.579529",
                },
                final_payload: {
                  result_text: resultText,
                  slackbot_streamed_answer_chars: streamedPrefix.length,
                },
              },
            ],
          });
        }
        if (url.pathname === "/agent/final-deliveries/exe-cutoff/delivered") {
          return jsonResponse({ ok: true });
        }
        throw new Error(`unexpected request: ${url.pathname}`);
      },
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const slackCalls: Array<{ method: string; params: any }> = [];
    const client = {
      chat: {
        postMessage: async (params: any) => {
          slackCalls.push({ method: "chat.postMessage", params });
          return { ok: true };
        },
      },
      conversations: {
        replies: async () => ({ ok: true, messages: [] }),
      },
    };

    try {
      await pollFinalDeliveriesOnce(config, client as any);

      const posts = slackCalls.filter(
        (call) => call.method === "chat.postMessage",
      );
      expect(posts).toHaveLength(1);
      const post = posts[0];
      if (!post) throw new Error("expected one Slack post");
      expect(post.params.text).toBe(
        "This is the report section that was cut off.",
      );
      expect(post.params.text).not.toContain("Already visible in Slack");
      expect(
        fetchCalls.some(
          (call) => call.path === "/agent/final-deliveries/exe-cutoff/delivered",
        ),
      ).toBe(true);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("rewrites Slack archive markdown links to app deep links for final delivery", async () => {
    const originalFetch = globalThis.fetch;
    const slackCalls: Array<{ method: string; params: any }> = [];
    const fetchMock = mock(
      async (input: string | URL | Request, _init?: RequestInit) => {
        const url = new URL(input instanceof Request ? input.url : input);
        if (url.pathname === "/agent/final-deliveries/claim") {
          return jsonResponse({
            deliveries: [
              {
                execution_id: "exe-slack-link",
                thread_key: "slack:T123:C999:1778883099.579529",
                delivery: {
                  platform: "slack",
                  channel: "C999",
                  thread_ts: "1778883099.579529",
                  recipient_team_id: "T123",
                },
                final_payload: {
                  result_text:
                    "See [thread](https://tempoxyz.enterprise.slack.com/archives/C456/p1779828836722119?thread_ts=1779828827.919439&channel=C456&message_ts=1779828836.722119)",
                },
              },
            ],
          });
        }
        if (url.pathname === "/agent/final-deliveries/exe-slack-link/delivered")
          return jsonResponse({ ok: true });
        throw new Error(`unexpected request: ${url.pathname}`);
      },
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const client = {
      chat: {
        postMessage: async (params: any) => {
          slackCalls.push({ method: "chat.postMessage", params });
          return { ok: true };
        },
      },
      conversations: {
        replies: async () => ({ ok: true, messages: [] }),
      },
    };

    try {
      await pollFinalDeliveriesOnce(
        { ...config, SLACK_TEAM_ID: "T123" },
        client as any,
      );
      const postMessage = slackCalls.find(
        (call) => call.method === "chat.postMessage",
      );
      const expected =
        "See [thread](slack://channel?team=T123&id=C456&message=1779828836.722119&thread_ts=1779828827.919439)";
      expect(postMessage?.params.text).toBe(expected);
      expect(postMessage?.params.blocks).toEqual([
        { type: "markdown", text: expected },
      ]);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("leaves Slack archive links unchanged when SLACK_TEAM_ID is not configured", async () => {
    const originalFetch = globalThis.fetch;
    const slackCalls: Array<{ method: string; params: any }> = [];
    const archiveLink =
      "https://tempoxyz.enterprise.slack.com/archives/C456/p1779828836722119?thread_ts=1779828827.919439&channel=C456&message_ts=1779828836.722119";
    const fetchMock = mock(
      async (input: string | URL | Request, _init?: RequestInit) => {
        const url = new URL(input instanceof Request ? input.url : input);
        if (url.pathname === "/agent/final-deliveries/claim") {
          return jsonResponse({
            deliveries: [
              {
                execution_id: "exe-slack-link-fallback",
                thread_key: "slack:T123:C999:1778883099.579529",
                delivery: {
                  platform: "slack",
                  channel: "C999",
                  thread_ts: "1778883099.579529",
                  recipient_team_id: "T123",
                },
                final_payload: {
                  result_text: `See [thread](${archiveLink})`,
                },
              },
            ],
          });
        }
        if (
          url.pathname ===
          "/agent/final-deliveries/exe-slack-link-fallback/delivered"
        )
          return jsonResponse({ ok: true });
        throw new Error(`unexpected request: ${url.pathname}`);
      },
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const client = {
      chat: {
        postMessage: async (params: any) => {
          slackCalls.push({ method: "chat.postMessage", params });
          return { ok: true };
        },
      },
      conversations: {
        replies: async () => ({ ok: true, messages: [] }),
      },
    };

    try {
      await pollFinalDeliveriesOnce(config, client as any);
      const postMessage = slackCalls.find(
        (call) => call.method === "chat.postMessage",
      );
      const expected = `See [thread](${archiveLink})`;
      expect(postMessage?.params.text).toBe(expected);
      expect(postMessage?.params.blocks).toEqual([
        { type: "markdown", text: expected },
      ]);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}
