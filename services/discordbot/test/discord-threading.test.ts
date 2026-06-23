import { describe, expect, it } from "bun:test";
import type { Logger } from "chat";
import {
  deriveThreadName,
  isThreadCreatedForMessage,
  renameThreadFromMessage,
} from "../src/discord-threading";
import type { DiscordbotFetch } from "../src/types";
import type { DiscordbotOptions } from "../src/types";

const silentLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => silentLogger,
};

describe("deriveThreadName", () => {
  it("strips a leading user mention", () => {
    expect(
      deriveThreadName("<@123456> deploy the staging app", "centaur"),
    ).toBe("deploy the staging app");
  });

  it("strips nickname and role mentions", () => {
    expect(deriveThreadName("<@!123> <@&456> check the logs", "centaur")).toBe(
      "check the logs",
    );
  });

  it("falls back when only a mention is present", () => {
    expect(deriveThreadName("<@123>", "centaur")).toBe("Centaur task");
  });

  it("clips to Discord’s 100-char thread-name limit", () => {
    const long = "a".repeat(200);
    const name = deriveThreadName(long, "centaur");
    expect(name.length).toBe(100);
  });
});

describe("isThreadCreatedForMessage", () => {
  it("is true when the thread was spawned from the triggering message", () => {
    // Discord gives a thread the id of the message it was created from.
    expect(isThreadCreatedForMessage("discord:G1:C1:M9", "M9")).toBe(true);
  });

  it("is false when the message arrived in a pre-existing thread", () => {
    expect(isThreadCreatedForMessage("discord:G1:C1:T9", "M9")).toBe(false);
  });

  it("is false when the key carries no thread segment", () => {
    expect(isThreadCreatedForMessage("discord:G1:C1", "M9")).toBe(false);
  });
});

describe("renameThreadFromMessage", () => {
  function options(fetchFn: DiscordbotFetch): DiscordbotOptions {
    return {
      apiUrl: "http://localhost",
      applicationId: "app",
      botToken: "bot-token",
      publicKey: "key",
      discordApiUrl: "https://discord.com/api/v10",
      fetch: fetchFn,
    };
  }

  it("PATCHes the thread channel with the new name", async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    const fetchFn = (async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return new Response("{}", { status: 200 });
    }) as DiscordbotFetch;

    await renameThreadFromMessage(
      options(fetchFn),
      "discord:G1:C1:T9",
      "deploy app",
      silentLogger,
    );

    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe("https://discord.com/api/v10/channels/T9");
    expect(calls[0]?.init?.method).toBe("PATCH");
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
      name: "deploy app",
    });
    const headers = calls[0]?.init?.headers as Record<string, string>;
    expect(headers.authorization).toBe("Bot bot-token");
  });

  it("no-ops when the key has no thread segment", async () => {
    let called = false;
    const fetchFn = (async () => {
      called = true;
      return new Response("{}");
    }) as DiscordbotFetch;

    await renameThreadFromMessage(
      options(fetchFn),
      "discord:G1:C1",
      "x",
      silentLogger,
    );
    expect(called).toBe(false);
  });

  it("swallows fetch errors", async () => {
    const fetchFn = (async () => {
      throw new Error("network down");
    }) as DiscordbotFetch;

    await expect(
      renameThreadFromMessage(
        options(fetchFn),
        "discord:G1:C1:T9",
        "x",
        silentLogger,
      ),
    ).resolves.toBeUndefined();
  });
});
