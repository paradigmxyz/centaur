import { describe, expect, it } from "bun:test";
import type { Logger, Message } from "chat";
import {
  isAllowedDiscordMessage,
  isAllowedTriggerBotMessage,
  isGuildAllowlistEmpty,
  parseDiscordThreadKey,
} from "../src/discord-allowlist";
import type { DiscordbotOptions } from "../src/types";

const silentLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => silentLogger,
};

function message(overrides: {
  threadId: string;
  isBot?: boolean | "unknown";
  isMe?: boolean;
}): Message {
  return {
    id: "m1",
    threadId: overrides.threadId,
    isMention: true,
    author: {
      isBot: overrides.isBot ?? false,
      isMe: overrides.isMe ?? false,
      userId: "u1",
      userName: "alice",
      fullName: "Alice",
    },
  } as unknown as Message;
}

function options(
  overrides: Partial<DiscordbotOptions> = {},
): DiscordbotOptions {
  return {
    apiUrl: "http://localhost",
    applicationId: "app",
    botToken: "token",
    publicKey: "key",
    guildAllowlist: ["G1", "G2"],
    ...overrides,
  };
}

describe("parseDiscordThreadKey", () => {
  it("decodes guild/channel/thread", () => {
    expect(parseDiscordThreadKey("discord:G1:C1:T1")).toEqual({
      guildId: "G1",
      channelId: "C1",
      threadId: "T1",
    });
  });

  it("handles missing thread segment", () => {
    expect(parseDiscordThreadKey("discord:G1:C1")).toEqual({
      guildId: "G1",
      channelId: "C1",
      threadId: undefined,
    });
  });

  it("returns empty for non-discord keys", () => {
    expect(parseDiscordThreadKey("slack:C1:123")).toEqual({});
  });
});

describe("isAllowedDiscordMessage", () => {
  it("allows an allowlisted guild from a human", () => {
    const allowed = isAllowedDiscordMessage(
      message({ threadId: "discord:G1:C1:T1" }),
      options(),
      silentLogger,
    );
    expect(allowed).toBe(true);
  });

  it("denies DMs (guildId @me)", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:@me:C1" }),
        options(),
        silentLogger,
      ),
    ).toBe(false);
  });

  it("denies a guild not on the allowlist", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:G9:C1:T1" }),
        options(),
        silentLogger,
      ),
    ).toBe(false);
  });

  it("is fail-closed: empty allowlist denies everything", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:G1:C1:T1" }),
        options({ guildAllowlist: [] }),
        silentLogger,
      ),
    ).toBe(false);
  });

  it("denies bot-authored messages", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:G1:C1:T1", isBot: true }),
        options(),
        silentLogger,
      ),
    ).toBe(false);
  });

  it("denies the bot’s own messages", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:G1:C1:T1", isMe: true }),
        options(),
        silentLogger,
      ),
    ).toBe(false);
  });

  it("allows an allowlisted trigger bot through the bot gate", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:G1:C1:T1", isBot: true }),
        options({ triggerBotAllowlist: ["u1"] }),
        silentLogger,
      ),
    ).toBe(true);
  });

  it("still denies a bot not on the trigger allowlist", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:G1:C1:T1", isBot: true }),
        options({ triggerBotAllowlist: ["someone-else"] }),
        silentLogger,
      ),
    ).toBe(false);
  });

  it("still denies the bot’s own messages even when allowlisted", () => {
    expect(
      isAllowedDiscordMessage(
        message({ threadId: "discord:G1:C1:T1", isBot: true, isMe: true }),
        options({ triggerBotAllowlist: ["u1"] }),
        silentLogger,
      ),
    ).toBe(false);
  });
});

describe("isAllowedTriggerBotMessage", () => {
  const botMessage = (raw?: unknown) =>
    ({
      author: {
        fullName: "Sentry",
        isBot: true,
        isMe: false,
        userId: "bot-1",
        userName: "sentry",
      },
      raw,
    }) as Parameters<typeof isAllowedTriggerBotMessage>[0];

  it("is fail-closed with no allowlist", () => {
    expect(isAllowedTriggerBotMessage(botMessage(), undefined)).toBe(false);
    expect(isAllowedTriggerBotMessage(botMessage(), [])).toBe(false);
  });

  it("matches the author user id", () => {
    expect(isAllowedTriggerBotMessage(botMessage(), ["bot-1"])).toBe(true);
    expect(isAllowedTriggerBotMessage(botMessage(), ["bot-2"])).toBe(false);
  });

  it("matches the raw application_id and webhook_id", () => {
    expect(
      isAllowedTriggerBotMessage(botMessage({ application_id: "app-9" }), [
        "app-9",
      ]),
    ).toBe(true);
    expect(
      isAllowedTriggerBotMessage(botMessage({ webhook_id: "hook-7" }), [
        "hook-7",
      ]),
    ).toBe(true);
    expect(
      isAllowedTriggerBotMessage(botMessage({ application_id: "app-9" }), [
        "other",
      ]),
    ).toBe(false);
  });

  it("tolerates entries and ids with surrounding whitespace", () => {
    expect(isAllowedTriggerBotMessage(botMessage(), [" bot-1 "])).toBe(true);
  });
});

describe("isGuildAllowlistEmpty", () => {
  it("is true when no guilds are configured", () => {
    expect(isGuildAllowlistEmpty(options({ guildAllowlist: [] }))).toBe(true);
  });

  it("is false when guilds are configured", () => {
    expect(isGuildAllowlistEmpty(options())).toBe(false);
  });
});
