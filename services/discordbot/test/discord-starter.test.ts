import { describe, expect, it } from "bun:test";
import type { Logger } from "chat";
import {
  fetchThreadStarterMessage,
  withDiscordEmbedText,
} from "../src/discord-starter";
import type { DiscordbotFetch, DiscordbotOptions } from "../src/types";

const silentLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => silentLogger,
};

function options(fetchFn: DiscordbotFetch): DiscordbotOptions {
  return {
    apiUrl: "http://localhost",
    applicationId: "bot-user",
    botToken: "bot-token",
    publicKey: "key",
    discordApiUrl: "https://discord.com/api/v10",
    fetch: fetchFn,
  };
}

// Shape of a Sentry-style webhook alert: empty content, payload in embeds.
const sentryStarter = {
  id: "T9",
  channel_id: "C1",
  content: "",
  timestamp: "2026-06-05T12:00:00.000000+00:00",
  author: { id: "webhook-1", username: "Sentry", bot: true },
  attachments: [],
  embeds: [
    {
      title: "TypeError: cannot read pair",
      url: "https://sentry.io/issues/123",
      description: "ingest.pairs in processSwap",
      fields: [{ name: "events", value: "14" }],
      footer: { text: "prod | ingest" },
    },
  ],
};

describe("fetchThreadStarterMessage", () => {
  it("fetches the starter from the parent channel and flattens embeds", async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    const fetchFn = (async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return new Response(JSON.stringify(sentryStarter), { status: 200 });
    }) as DiscordbotFetch;

    const starter = await fetchThreadStarterMessage(
      options(fetchFn),
      "discord:G1:C1:T9",
      silentLogger,
    );

    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe(
      "https://discord.com/api/v10/channels/C1/messages/T9",
    );
    const headers = calls[0]?.init?.headers as Record<string, string>;
    expect(headers.authorization).toBe("Bot bot-token");

    expect(starter).not.toBeNull();
    expect(starter?.id).toBe("T9");
    expect(starter?.threadId).toBe("discord:G1:C1:T9");
    expect(starter?.author.userName).toBe("Sentry");
    expect(starter?.author.isBot).toBe(true);
    expect(starter?.author.isMe).toBe(false);
    expect(starter?.text).toContain(
      "TypeError: cannot read pair (https://sentry.io/issues/123)",
    );
    expect(starter?.text).toContain("ingest.pairs in processSwap");
    expect(starter?.text).toContain("events: 14");
    expect(starter?.text).toContain("prod | ingest");
  });

  it("keeps plain content and maps attachments", async () => {
    const fetchFn = (async () =>
      new Response(
        JSON.stringify({
          id: "T9",
          content: "look at this",
          timestamp: "2026-06-05T12:00:00.000000+00:00",
          author: { id: "U1", username: "will", global_name: "Will" },
          attachments: [
            {
              filename: "chart.png",
              content_type: "image/png",
              url: "https://cdn.discordapp.com/chart.png",
              size: 123,
              width: 800,
              height: 600,
            },
          ],
          embeds: [],
        }),
        { status: 200 },
      )) as DiscordbotFetch;

    const starter = await fetchThreadStarterMessage(
      options(fetchFn),
      "discord:G1:C1:T9",
      silentLogger,
    );

    expect(starter?.text).toBe("look at this");
    expect(starter?.author.fullName).toBe("Will");
    expect(starter?.attachments).toEqual([
      {
        height: 600,
        mimeType: "image/png",
        name: "chart.png",
        size: 123,
        type: "image",
        url: "https://cdn.discordapp.com/chart.png",
        width: 800,
      },
    ]);
  });

  it("returns null on 404 (thread not created from a message)", async () => {
    const fetchFn = (async () =>
      new Response('{"message": "Unknown Message", "code": 10008}', {
        status: 404,
      })) as DiscordbotFetch;

    const starter = await fetchThreadStarterMessage(
      options(fetchFn),
      "discord:G1:C1:T9",
      silentLogger,
    );
    expect(starter).toBeNull();
  });

  it("no-ops when the key has no thread segment", async () => {
    let called = false;
    const fetchFn = (async () => {
      called = true;
      return new Response("{}");
    }) as DiscordbotFetch;

    const starter = await fetchThreadStarterMessage(
      options(fetchFn),
      "discord:G1:C1",
      silentLogger,
    );
    expect(starter).toBeNull();
    expect(called).toBe(false);
  });

  it("swallows fetch errors", async () => {
    const fetchFn = (async () => {
      throw new Error("network down");
    }) as DiscordbotFetch;

    await expect(
      fetchThreadStarterMessage(
        options(fetchFn),
        "discord:G1:C1:T9",
        silentLogger,
      ),
    ).resolves.toBeNull();
  });
});

describe("withDiscordEmbedText", () => {
  it("appends embed text after existing content", () => {
    const text = withDiscordEmbedText("heads up", {
      embeds: [{ title: "Alert", description: "something broke" }],
    });
    expect(text).toBe("heads up\n\n[embed] Alert\nsomething broke");
  });

  it("returns the text unchanged without embeds", () => {
    expect(withDiscordEmbedText("plain", { embeds: [] })).toBe("plain");
    expect(withDiscordEmbedText("plain", undefined)).toBe("plain");
    expect(withDiscordEmbedText("plain", "not-an-object")).toBe("plain");
  });

  it("skips malformed embeds and fields", () => {
    const text = withDiscordEmbedText("", {
      embeds: [null, {}, { fields: [null, { value: "lonely value" }] }],
    });
    expect(text).toBe("[embed] lonely value");
  });
});
