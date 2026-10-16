import { describe, expect, it } from "bun:test";
import type { Logger } from "chat";
import {
  conversationName,
  isAllowedTelegramMessage,
  isAllowlistEmpty,
  isReplyToBot,
  isStorableTelegramMessage,
  mentionsBot,
  messageTrigger,
  parseTelegramThreadKey,
  routeCommand,
  type TelegramPolicyMessage,
  withReplyQuote,
} from "../src/telegram-policy";

const BOT_ID = "900";
const USER = { first_name: "Ada", id: 42, is_bot: false, username: "ada" };
const GROUP = { id: -100123, title: "Ops", type: "supergroup" as const };
const OPTIONS = { chatAllowlist: ["-100123"], userAllowlist: ["42"] };

function recordingLogger(): Logger & { events: string[] } {
  const events: string[] = [];
  const logger = {
    events,
    debug: () => undefined,
    info: (event: string) => void events.push(event),
    warn: (event: string) => void events.push(event),
    error: (event: string) => void events.push(event),
    child: () => logger,
  };
  return logger;
}

function dm(
  overrides: Partial<TelegramPolicyMessage> = {},
): TelegramPolicyMessage {
  return {
    chat: { first_name: "Ada", id: 42, last_name: "Lovelace", type: "private" },
    date: 1,
    from: USER,
    message_id: 1,
    text: "hi",
    ...overrides,
  };
}

function group(
  overrides: Partial<TelegramPolicyMessage> = {},
): TelegramPolicyMessage {
  return {
    chat: GROUP,
    date: 1,
    from: USER,
    message_id: 2,
    text: "hi",
    ...overrides,
  };
}

const botReply = {
  chat: GROUP,
  date: 1,
  from: { first_name: "Centaur", id: 900, is_bot: true },
  message_id: 10,
  text: "earlier answer",
};

describe("isAllowedTelegramMessage", () => {
  const allowed = (message: TelegramPolicyMessage, options = OPTIONS) => {
    const logger = recordingLogger();
    return {
      ok: isAllowedTelegramMessage(message, options, BOT_ID, logger),
      events: logger.events,
    };
  };

  it("allows allowlisted DMs and groups", () => {
    expect(allowed(dm()).ok).toBe(true);
    expect(allowed(group()).ok).toBe(true);
  });

  it("denies DMs per user and groups per chat", () => {
    expect(
      allowed(
        dm({ from: { ...USER, id: 7 }, chat: { id: 7, type: "private" } }),
      ).events,
    ).toEqual(["telegrambot_message_ignored_user_not_allowlisted"]);
    expect(allowed(group({ chat: { ...GROUP, id: -1009 } })).events).toEqual([
      "telegrambot_message_ignored_chat_not_allowlisted",
    ]);
  });

  it("requires group senders to be on the user allowlist too", () => {
    expect(allowed(group({ from: { ...USER, id: 7 } })).events).toEqual([
      "telegrambot_message_ignored_user_not_allowlisted",
    ]);
  });

  it("does not let either allowlist alone open DMs or groups", () => {
    expect(
      allowed(dm(), { chatAllowlist: ["-100123"], userAllowlist: [] }).ok,
    ).toBe(false);
    expect(
      allowed(group(), { chatAllowlist: [], userAllowlist: ["42"] }).ok,
    ).toBe(false);
  });

  it("is inert with empty allowlists", () => {
    const empty = { chatAllowlist: [], userAllowlist: [] };
    expect(isAllowlistEmpty(empty)).toBe(true);
    expect(isAllowlistEmpty({})).toBe(true);
    expect(isAllowlistEmpty(OPTIONS)).toBe(false);
    expect(allowed(dm(), empty).events).toEqual([
      "telegrambot_message_ignored_allowlist_empty",
    ]);
    expect(allowed(group(), empty).events).toEqual([
      "telegrambot_message_ignored_allowlist_empty",
    ]);
  });

  it("denies self, bots, relays, channel identities, edits, and channels", () => {
    const cases: [TelegramPolicyMessage, string[]][] = [
      [dm({ from: { first_name: "Centaur", id: 900, is_bot: true } }), []],
      [
        group({ from: { first_name: "B", id: 5, is_bot: true } }),
        ["telegrambot_message_ignored_bot_author"],
      ],
      [
        group({ via_bot: { first_name: "I", id: 6, is_bot: true } }),
        ["telegrambot_message_ignored_via_bot"],
      ],
      [
        group({ sender_chat: { id: -1007, type: "channel" } }),
        ["telegrambot_message_ignored_sender_chat"],
      ],
      // Anonymous group admin: from is GroupAnonymousBot, sender_chat is the group.
      [
        group({
          from: { first_name: "Group", id: 1087968824, is_bot: true },
          sender_chat: GROUP,
        }),
        ["telegrambot_message_ignored_sender_chat"],
      ],
      [
        group({ is_automatic_forward: true }),
        ["telegrambot_message_ignored_automatic_forward"],
      ],
      [
        group({ from: undefined }),
        ["telegrambot_message_ignored_missing_sender"],
      ],
      [dm({ edit_date: 2 }), ["telegrambot_message_ignored_edit"]],
      [
        { ...group(), chat: { id: -100123, type: "channel" } },
        ["telegrambot_message_ignored_unsupported_chat_type"],
      ],
    ];
    for (const [message, events] of cases) {
      const result = allowed(message);
      expect(result.ok).toBe(false);
      expect(result.events).toEqual(events);
    }
  });
});

describe("storage", () => {
  const OPTIONS_ = { chatAllowlist: ["-100123"], userAllowlist: ["42"] };

  it("keeps every member's message in allowlisted groups and allowlisted DMs only", () => {
    expect(isStorableTelegramMessage(group(), OPTIONS_, BOT_ID)).toBe(true);
    expect(
      isStorableTelegramMessage(
        group({ from: { ...USER, id: 7 } }),
        OPTIONS_,
        BOT_ID,
      ),
    ).toBe(true);
    expect(isStorableTelegramMessage(dm(), OPTIONS_, BOT_ID)).toBe(true);
  });

  it("keeps nothing from other chats, DMs, edits, or the bot itself", () => {
    expect(
      isStorableTelegramMessage(
        group({ chat: { id: -1009, title: "Other", type: "supergroup" } }),
        OPTIONS_,
        BOT_ID,
      ),
    ).toBe(false);
    expect(
      isStorableTelegramMessage(
        dm({ from: { ...USER, id: 7 }, chat: { id: 7, type: "private" } }),
        OPTIONS_,
        BOT_ID,
      ),
    ).toBe(false);
    expect(
      isStorableTelegramMessage(group({ edit_date: 1 }), OPTIONS_, BOT_ID),
    ).toBe(false);
    expect(
      isStorableTelegramMessage(
        group({ from: { first_name: "C", id: 900, is_bot: true } }),
        OPTIONS_,
        BOT_ID,
      ),
    ).toBe(false);
  });
});

describe("triggers", () => {
  it("treats a plain @mention as a trigger only when mentions are enabled", () => {
    const mention = group({
      text: "hey @Centaur_Bot hi",
      entities: [{ type: "mention", offset: 4, length: 12 }],
    });
    expect(messageTrigger(mention, BOT_ID)).toBeNull();
    expect(
      messageTrigger(mention, BOT_ID, { botUserName: "centaur_bot" }),
    ).toBe("mention");
    // DMs and replies keep their own trigger.
    expect(messageTrigger(dm(), BOT_ID, { botUserName: "centaur_bot" })).toBe(
      "dm",
    );
  });

  it("matches mentions by Telegram entity, not by text", () => {
    // Another bot.
    expect(
      mentionsBot(
        group({
          text: "@other_bot hi",
          entities: [{ type: "mention", offset: 0, length: 10 }],
        }),
        BOT_ID,
        "centaur_bot",
      ),
    ).toBe(false);
    // The handle inside inline code has no mention entity.
    expect(
      mentionsBot(
        group({
          text: "`@centaur_bot`",
          entities: [{ type: "code", offset: 0, length: 14 }],
        }),
        BOT_ID,
        "centaur_bot",
      ),
    ).toBe(false);
    // A text_mention of the bot's user id.
    expect(
      mentionsBot(
        group({
          text: "Centaur help",
          entities: [
            {
              type: "text_mention",
              offset: 0,
              length: 7,
              user: { first_name: "Centaur", id: 900, is_bot: true },
            },
          ],
        }),
        BOT_ID,
        "centaur_bot",
      ),
    ).toBe(true);
  });


  it("triggers on every DM and on group replies to the bot only", () => {
    expect(messageTrigger(dm(), BOT_ID)).toBe("dm");
    expect(messageTrigger(group(), BOT_ID)).toBeNull();
    expect(
      messageTrigger(
        group({
          text: "@centaur_bot hi",
          entities: [{ type: "mention", offset: 0, length: 12 }],
        }),
        BOT_ID,
      ),
    ).toBeNull();
    expect(messageTrigger(group({ reply_to_message: botReply }), BOT_ID)).toBe(
      "reply",
    );
    expect(
      messageTrigger(
        group({ reply_to_message: { ...botReply, from: USER } }),
        BOT_ID,
      ),
    ).toBeNull();
  });

  it("ignores the implicit forum-topic reply to a bot-created topic", () => {
    const topicRoot = { ...botReply, message_id: 55 };
    expect(
      isReplyToBot(
        group({ message_thread_id: 55, reply_to_message: topicRoot }),
        BOT_ID,
      ),
    ).toBe(false);
    expect(
      isReplyToBot(
        group({ message_thread_id: 55, reply_to_message: botReply }),
        BOT_ID,
      ),
    ).toBe(true);
    expect(isReplyToBot(group({ reply_to_message: botReply }), undefined)).toBe(
      false,
    );
  });

  it("requires an addressed command in groups", () => {
    const command = (text: string, chat: "dm" | "group") => {
      const entity = [
        { type: "bot_command", offset: 0, length: text.split(" ")[0]!.length },
      ];
      return chat === "dm"
        ? dm({ text, entities: entity })
        : group({ text, entities: entity });
    };
    expect(routeCommand(command("/ask@centaur_bot hi", "group"), "/ask")).toBe(
      "ask",
    );
    expect(routeCommand(command("/ask hi", "group"), "/ask")).toBe("ignore");
    expect(routeCommand(command("/ask hi", "dm"), "/ask")).toBe("ask");
    expect(routeCommand(command("/start", "dm"), "/start")).toBe("help");
    expect(routeCommand(command("/help@centaur_bot", "group"), "/help")).toBe(
      "help",
    );
    expect(routeCommand(command("/help", "group"), "/help")).toBe("ignore");
  });
});

describe("helpers", () => {
  it("parses only well-formed adapter thread keys", () => {
    expect(parseTelegramThreadKey("telegram:42")).toEqual({
      chatId: "42",
      topicId: undefined,
    });
    expect(parseTelegramThreadKey("telegram:-100123:7")).toEqual({
      chatId: "-100123",
      topicId: "7",
    });
    for (const key of [
      "telegram:",
      "telegram:abc",
      "telegram:biz:c:42",
      "telegram:1:x",
      "discord:1:2",
    ]) {
      expect(parseTelegramThreadKey(key)).toEqual({});
    }
  });

  it("quotes another member's message but never the bot's own", () => {
    const other = {
      ...botReply,
      from: { first_name: "Grace", id: 7, is_bot: false, username: "grace" },
      text: "line 1\nline 2",
    };
    expect(
      withReplyQuote("why?", group({ reply_to_message: other }), BOT_ID),
    ).toBe("In reply to grace:\n> line 1\n> line 2\n\nwhy?");
    expect(
      withReplyQuote(
        "why?",
        group({ reply_to_message: other, quote: { text: "line 2" } }),
        BOT_ID,
      ),
    ).toBe("In reply to grace:\n> line 2\n\nwhy?");
    expect(
      withReplyQuote("why?", group({ reply_to_message: botReply }), BOT_ID),
    ).toBe("why?");
    expect(withReplyQuote("why?", group(), BOT_ID)).toBe("why?");
  });

  it("names the conversation from the group title or the DM user", () => {
    expect(conversationName(group())).toBe("Ops");
    expect(conversationName(dm())).toBe("Ada Lovelace");
  });
});
