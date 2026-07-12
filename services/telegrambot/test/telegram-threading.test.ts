import { describe, expect, test } from "bun:test";
import {
  deriveClientMessageId,
  deriveConversationName,
  deriveThreadKey,
  parseTelegramThreadKey,
} from "../src/telegram-threading";
import type {
  TelegramChat,
  TelegramMessage,
  TelegramUser,
} from "../src/types";

const BOT_USER_ID = "999";

function user(overrides: Partial<TelegramUser> = {}): TelegramUser {
  return { id: 42, is_bot: false, first_name: "Alice", ...overrides };
}

function privateMessage(
  overrides: Partial<TelegramMessage> = {},
): TelegramMessage {
  return {
    message_id: 10,
    date: 0,
    chat: { id: 42, type: "private", first_name: "Alice" },
    from: user(),
    text: "hello",
    ...overrides,
  };
}

function groupMessage(
  overrides: Partial<TelegramMessage> = {},
): TelegramMessage {
  return {
    message_id: 11,
    date: 0,
    chat: { id: -1005000, type: "supergroup", title: "Team" },
    from: user(),
    text: "hello",
    ...overrides,
  };
}

function expectThreadKey(
  result: ReturnType<typeof deriveThreadKey>,
  threadKey: string,
): void {
  expect(result).toEqual({ threadKey });
}

function expectRejected(
  result: ReturnType<typeof deriveThreadKey>,
  rejected: string,
): void {
  expect(result).toEqual({ rejected });
}

describe("deriveThreadKey", () => {
  test("private chat without a topic", () => {
    expectThreadKey(
      deriveThreadKey(privateMessage(), BOT_USER_ID),
      "telegram:private:42",
    );
  });

  test("private chat preserves a forum-mode message_thread_id", () => {
    expectThreadKey(
      deriveThreadKey(privateMessage({ message_thread_id: 7 }), BOT_USER_ID),
      "telegram:private:42:7",
    );
  });

  test("supergroup without a topic", () => {
    expectThreadKey(
      deriveThreadKey(groupMessage(), BOT_USER_ID),
      "telegram:chat:-1005000",
    );
  });

  test("plain group uses the same telegram:chat: kind as supergroups", () => {
    const message = groupMessage({
      chat: { id: -333, type: "group", title: "Small" },
    });
    expectThreadKey(
      deriveThreadKey(message, BOT_USER_ID),
      "telegram:chat:-333",
    );
  });

  test("forum topic message extends the group key", () => {
    const message = groupMessage({
      chat: { id: -1005000, type: "supergroup", title: "Team", is_forum: true },
      is_topic_message: true,
      message_thread_id: 55,
    });
    expectThreadKey(
      deriveThreadKey(message, BOT_USER_ID),
      "telegram:chat:-1005000:55",
    );
  });

  test("non-topic reply-chain message_thread_id does not fragment the group key", () => {
    // Non-forum supergroup replies carry message_thread_id (the reply root);
    // reply-to-bot follow-ups must land in the same chat session.
    const message = groupMessage({ message_thread_id: 88 });
    expectThreadKey(
      deriveThreadKey(message, BOT_USER_ID),
      "telegram:chat:-1005000",
    );
  });

  test("rejects channel chats", () => {
    const message = groupMessage({
      chat: { id: -1007000, type: "channel", title: "News" },
      from: undefined,
      sender_chat: { id: -1007000, type: "channel", title: "News" },
    });
    expectRejected(deriveThreadKey(message, BOT_USER_ID), "channel_chat");
  });

  test("rejects the bot's own messages", () => {
    const message = groupMessage({
      from: user({ id: Number(BOT_USER_ID), is_bot: true }),
    });
    expectRejected(deriveThreadKey(message, BOT_USER_ID), "self_message");
  });

  describe("private identity validation", () => {
    test("rejects a missing sender", () => {
      expectRejected(
        deriveThreadKey(privateMessage({ from: undefined }), BOT_USER_ID),
        "private_missing_from",
      );
    });

    test("rejects a bot sender", () => {
      expectRejected(
        deriveThreadKey(
          privateMessage({ from: user({ is_bot: true }) }),
          BOT_USER_ID,
        ),
        "private_bot_sender",
      );
    });

    test("rejects when sender_chat is present", () => {
      const message = privateMessage({
        sender_chat: { id: -1007000, type: "channel", title: "News" },
      });
      expectRejected(
        deriveThreadKey(message, BOT_USER_ID),
        "private_sender_chat_present",
      );
    });

    test("rejects when from.id differs from chat.id (Business/DM-topic shapes)", () => {
      expectRejected(
        deriveThreadKey(privateMessage({ from: user({ id: 43 }) }), BOT_USER_ID),
        "private_identity_mismatch",
      );
    });
  });
});

describe("parseTelegramThreadKey", () => {
  test("round-trips a private key with a topic", () => {
    const derived = deriveThreadKey(
      privateMessage({ message_thread_id: 7 }),
      BOT_USER_ID,
    );
    if (!("threadKey" in derived)) throw new Error("expected a thread key");
    expect(parseTelegramThreadKey(derived.threadKey)).toEqual({
      kind: "private",
      chatId: "42",
      messageThreadId: 7,
    });
  });

  test("round-trips a group key without a topic", () => {
    const derived = deriveThreadKey(groupMessage(), BOT_USER_ID);
    if (!("threadKey" in derived)) throw new Error("expected a thread key");
    expect(parseTelegramThreadKey(derived.threadKey)).toEqual({
      kind: "chat",
      chatId: "-1005000",
    });
  });

  test("parses negative chat ids with topic segments", () => {
    expect(parseTelegramThreadKey("telegram:chat:-1005000:55")).toEqual({
      kind: "chat",
      chatId: "-1005000",
      messageThreadId: 55,
    });
  });

  test("returns empty for non-telegram keys", () => {
    expect(parseTelegramThreadKey("discord:G1:C1")).toEqual({});
  });

  test("returns empty for an unknown kind discriminator", () => {
    expect(parseTelegramThreadKey("telegram:channel:-1")).toEqual({});
  });

  test("returns empty for malformed topic or extra segments", () => {
    expect(parseTelegramThreadKey("telegram:chat:-1:abc")).toEqual({});
    expect(parseTelegramThreadKey("telegram:chat:-1:2:3")).toEqual({});
    expect(parseTelegramThreadKey("telegram:chat:")).toEqual({});
  });
});

describe("deriveConversationName", () => {
  const chat = (overrides: Partial<TelegramChat>): TelegramChat => ({
    id: 42,
    type: "private",
    ...overrides,
  });

  test("uses the title for groups", () => {
    expect(
      deriveConversationName(
        chat({ id: -1, type: "supergroup", title: "Team" }),
      ),
    ).toBe("Team");
  });

  test("falls back to the id for an untitled group", () => {
    expect(deriveConversationName(chat({ id: -1, type: "group" }))).toBe("-1");
  });

  test("composes first/last/username for private chats", () => {
    expect(
      deriveConversationName(
        chat({ first_name: "Alice", last_name: "Smith", username: "alice" }),
      ),
    ).toBe("Alice Smith (@alice)");
  });

  test("uses the bare name when there is no username", () => {
    expect(deriveConversationName(chat({ first_name: "Alice" }))).toBe(
      "Alice",
    );
  });

  test("uses the username when there is no name", () => {
    expect(deriveConversationName(chat({ username: "alice" }))).toBe("@alice");
  });

  test("falls back to the id for private chats", () => {
    expect(deriveConversationName(chat({}))).toBe("42");
  });
});

describe("deriveClientMessageId", () => {
  test("is stable per chat and message", () => {
    expect(deriveClientMessageId(groupMessage())).toBe(
      "telegram:-1005000:11",
    );
    expect(deriveClientMessageId(privateMessage())).toBe("telegram:42:10");
  });
});
