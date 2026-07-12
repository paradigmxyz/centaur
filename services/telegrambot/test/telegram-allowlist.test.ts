import { afterEach, describe, expect, test } from "bun:test";
import {
  extractCommandText,
  isAllowedTelegramMessage,
  isChatAllowlistEmpty,
  isTriggerMessage,
  isUserAllowlistEmpty,
  resolveChatAllowlist,
  resolveUserAllowlist,
  type TelegramAllowlistOptions,
} from "../src/telegram-allowlist";
import type {
  TelegramChat,
  TelegramMessage,
  TelegramUser,
} from "../src/types";
import { noopLogger } from "../src/utils";

const BOT_USER_ID = "999";
const BOT_USERNAME = "centaur_bot";
const GROUP_CHAT_ID = -1005000;
const DM_USER_ID = 42;

function user(overrides: Partial<TelegramUser> = {}): TelegramUser {
  return { id: DM_USER_ID, is_bot: false, first_name: "Alice", ...overrides };
}

function privateChat(overrides: Partial<TelegramChat> = {}): TelegramChat {
  return { id: DM_USER_ID, type: "private", first_name: "Alice", ...overrides };
}

function groupChat(overrides: Partial<TelegramChat> = {}): TelegramChat {
  return { id: GROUP_CHAT_ID, type: "supergroup", title: "Team", ...overrides };
}

function dmMessage(overrides: Partial<TelegramMessage> = {}): TelegramMessage {
  return {
    message_id: 1,
    date: 0,
    chat: privateChat(),
    from: user(),
    text: "hello",
    ...overrides,
  };
}

function groupMessage(
  overrides: Partial<TelegramMessage> = {},
): TelegramMessage {
  return {
    message_id: 2,
    date: 0,
    chat: groupChat(),
    from: user(),
    text: "hello",
    ...overrides,
  };
}

/** Text starting with a Telegram-vouched bot_command entity at offset 0. */
function commandMessage(
  base: TelegramMessage,
  text: string,
  commandLength: number,
): TelegramMessage {
  return {
    ...base,
    text,
    entities: [{ type: "bot_command", offset: 0, length: commandLength }],
  };
}

function options(
  overrides: Partial<TelegramAllowlistOptions> = {},
): TelegramAllowlistOptions {
  return {
    botUserId: BOT_USER_ID,
    chatAllowlist: [String(GROUP_CHAT_ID)],
    userAllowlist: [String(DM_USER_ID)],
    ...overrides,
  };
}

describe("isAllowedTelegramMessage", () => {
  test("allows an allowlisted DM user", () => {
    expect(isAllowedTelegramMessage(dmMessage(), options(), noopLogger)).toBe(
      true,
    );
  });

  test("allows an allowlisted group chat", () => {
    expect(
      isAllowedTelegramMessage(groupMessage(), options(), noopLogger),
    ).toBe(true);
  });

  test("allows an allowlisted plain group (not only supergroups)", () => {
    const message = groupMessage({ chat: groupChat({ type: "group" }) });
    expect(isAllowedTelegramMessage(message, options(), noopLogger)).toBe(true);
  });

  test("is fail-closed: empty user allowlist denies every DM", () => {
    expect(
      isAllowedTelegramMessage(
        dmMessage(),
        options({ userAllowlist: [] }),
        noopLogger,
      ),
    ).toBe(false);
  });

  test("is fail-closed: empty chat allowlist denies every group", () => {
    expect(
      isAllowedTelegramMessage(
        groupMessage(),
        options({ chatAllowlist: [] }),
        noopLogger,
      ),
    ).toBe(false);
  });

  test("denies a DM user not on the allowlist", () => {
    const message = dmMessage({
      chat: privateChat({ id: 43 }),
      from: user({ id: 43 }),
    });
    expect(isAllowedTelegramMessage(message, options(), noopLogger)).toBe(
      false,
    );
  });

  test("denies a group chat not on the allowlist", () => {
    const message = groupMessage({ chat: groupChat({ id: -777 }) });
    expect(isAllowedTelegramMessage(message, options(), noopLogger)).toBe(
      false,
    );
  });

  test("denies the bot's own messages even in allowlisted chats", () => {
    const message = groupMessage({
      from: user({ id: Number(BOT_USER_ID), is_bot: true }),
    });
    expect(isAllowedTelegramMessage(message, options(), noopLogger)).toBe(
      false,
    );
  });

  test("denies other bots even in allowlisted chats", () => {
    const message = groupMessage({ from: user({ id: 55, is_bot: true }) });
    expect(isAllowedTelegramMessage(message, options(), noopLogger)).toBe(
      false,
    );
  });

  test("denies bot-relayed (via_bot) messages", () => {
    const message = dmMessage({
      via_bot: user({ id: 55, is_bot: true, username: "inline_bot" }),
    });
    expect(isAllowedTelegramMessage(message, options(), noopLogger)).toBe(
      false,
    );
  });

  test("denies messages without a sender", () => {
    const message = groupMessage({ from: undefined });
    expect(isAllowedTelegramMessage(message, options(), noopLogger)).toBe(
      false,
    );
  });

  test("denies channel chats regardless of allowlists", () => {
    const message = groupMessage({
      chat: { id: GROUP_CHAT_ID, type: "channel", title: "News" },
    });
    expect(
      isAllowedTelegramMessage(
        message,
        options({ chatAllowlist: [String(GROUP_CHAT_ID)] }),
        noopLogger,
      ),
    ).toBe(false);
  });
});

describe("isTriggerMessage", () => {
  test("any allowed DM message triggers as dm", () => {
    expect(isTriggerMessage(dmMessage(), BOT_USER_ID, BOT_USERNAME)).toBe("dm");
  });

  test("a bare /ask in a DM triggers as command", () => {
    const message = commandMessage(dmMessage(), "/ask what is up", 4);
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBe(
      "command",
    );
  });

  test("an addressed /ask@bot in a DM triggers as command", () => {
    const message = commandMessage(
      dmMessage(),
      `/ask@${BOT_USERNAME} what is up`,
      4 + 1 + BOT_USERNAME.length,
    );
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBe(
      "command",
    );
  });

  test("a reply to the bot's own message triggers in a group", () => {
    const message = groupMessage({
      reply_to_message: groupMessage({
        from: user({ id: Number(BOT_USER_ID), is_bot: true }),
      }),
    });
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBe("reply");
  });

  test("a reply to another user does not trigger in a group", () => {
    const message = groupMessage({
      reply_to_message: groupMessage({ from: user({ id: 77 }) }),
    });
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBeNull();
  });

  test("/ask@bot triggers in a group", () => {
    const message = commandMessage(
      groupMessage(),
      `/ask@${BOT_USERNAME} deploy the thing`,
      4 + 1 + BOT_USERNAME.length,
    );
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBe(
      "command",
    );
  });

  test("the @suffix comparison is case-insensitive", () => {
    const message = commandMessage(
      groupMessage(),
      "/ask@Centaur_Bot deploy",
      16,
    );
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBe(
      "command",
    );
  });

  test("a wrong-bot @suffix does not trigger in a group", () => {
    const message = commandMessage(groupMessage(), "/ask@other_bot hi", 14);
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBeNull();
  });

  test("a bare /ask does not trigger in a group", () => {
    const message = commandMessage(groupMessage(), "/ask hi", 4);
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBeNull();
  });

  test("/ask@bot text without a bot_command entity does not trigger", () => {
    const message = groupMessage({ text: `/ask@${BOT_USERNAME} hi` });
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBeNull();
  });

  test("a plain textual @mention does not trigger (privacy-mode contract)", () => {
    const message = groupMessage({
      text: `@${BOT_USERNAME} are you there?`,
      entities: [
        { type: "mention", offset: 0, length: BOT_USERNAME.length + 1 },
      ],
    });
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBeNull();
  });

  test("plain group chatter does not trigger", () => {
    expect(isTriggerMessage(groupMessage(), BOT_USER_ID, BOT_USERNAME)).toBe(
      null,
    );
  });

  test("channel messages never trigger", () => {
    const message = groupMessage({
      chat: { id: GROUP_CHAT_ID, type: "channel", title: "News" },
    });
    expect(isTriggerMessage(message, BOT_USER_ID, BOT_USERNAME)).toBeNull();
  });
});

describe("extractCommandText", () => {
  test("strips a bare /ask", () => {
    const message = commandMessage(dmMessage(), "/ask what is up", 4);
    expect(extractCommandText(message, BOT_USERNAME)).toBe("what is up");
  });

  test("strips an addressed /ask@bot", () => {
    const message = commandMessage(
      groupMessage(),
      `/ask@${BOT_USERNAME} deploy the thing`,
      4 + 1 + BOT_USERNAME.length,
    );
    expect(extractCommandText(message, BOT_USERNAME)).toBe("deploy the thing");
  });

  test("leaves a wrong-bot command intact", () => {
    const message = commandMessage(groupMessage(), "/ask@other_bot hi", 14);
    expect(extractCommandText(message, BOT_USERNAME)).toBe(
      "/ask@other_bot hi",
    );
  });

  test("passes free text through trimmed", () => {
    const message = dmMessage({ text: "  hello there  " });
    expect(extractCommandText(message, BOT_USERNAME)).toBe("hello there");
  });

  test("returns empty for a command with no arguments", () => {
    const message = commandMessage(dmMessage(), "/ask", 4);
    expect(extractCommandText(message, BOT_USERNAME)).toBe("");
  });
});

describe("allowlist resolution", () => {
  afterEach(() => {
    delete process.env.TELEGRAMBOT_CHAT_ALLOWLIST;
    delete process.env.TELEGRAMBOT_USER_ALLOWLIST;
  });

  test("isChatAllowlistEmpty / isUserAllowlistEmpty reflect the options", () => {
    expect(isChatAllowlistEmpty({ chatAllowlist: [] })).toBe(true);
    expect(isChatAllowlistEmpty({ chatAllowlist: ["-1"] })).toBe(false);
    expect(isUserAllowlistEmpty({ userAllowlist: [] })).toBe(true);
    expect(isUserAllowlistEmpty({ userAllowlist: ["42"] })).toBe(false);
  });

  test("falls back to env lists when options are unset", () => {
    process.env.TELEGRAMBOT_CHAT_ALLOWLIST = "-1001, -1002 -1003";
    process.env.TELEGRAMBOT_USER_ALLOWLIST = "42";
    expect(resolveChatAllowlist({})).toEqual(["-1001", "-1002", "-1003"]);
    expect(resolveUserAllowlist({})).toEqual(["42"]);
    expect(isChatAllowlistEmpty({})).toBe(false);
  });

  test("explicit options take precedence over env", () => {
    process.env.TELEGRAMBOT_USER_ALLOWLIST = "42";
    expect(resolveUserAllowlist({ userAllowlist: [] })).toEqual([]);
    expect(isUserAllowlistEmpty({ userAllowlist: [] })).toBe(true);
  });

  test("unset options and env mean inert", () => {
    expect(resolveChatAllowlist({})).toEqual([]);
    expect(resolveUserAllowlist({})).toEqual([]);
  });
});
