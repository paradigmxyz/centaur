import { describe, expect, it } from "bun:test";
import { hasContextContent, toContextMessage } from "../src/context-sink";
import {
  isIndexableTelegramMessage,
  type TelegramPolicyMessage,
} from "../src/telegram-policy";

const BOT_ID = "777";
const GROUP = { id: -100123, title: " Ops Room ", type: "supergroup" as const };

function message(fields: Partial<TelegramPolicyMessage>): TelegramPolicyMessage {
  return {
    chat: GROUP,
    date: 1_700_000_000,
    from: { first_name: "Ada", id: 11, is_bot: false, last_name: "L", username: "ada" },
    message_id: 5,
    ...fields,
  } as TelegramPolicyMessage;
}

describe("isIndexableTelegramMessage", () => {
  const options = { contextChatAllowlist: [String(GROUP.id)] };

  it("indexes every member of a context group only", () => {
    expect(isIndexableTelegramMessage(message({}), options, BOT_ID)).toBe(true);
    expect(
      isIndexableTelegramMessage(
        message({ from: { first_name: "Eve", id: 99, is_bot: false } }),
        options,
        BOT_ID,
      ),
    ).toBe(true);
    expect(isIndexableTelegramMessage(message({}), {}, BOT_ID)).toBe(false);
    expect(
      isIndexableTelegramMessage(
        message({ chat: { id: -100999, type: "supergroup" } }),
        options,
        BOT_ID,
      ),
    ).toBe(false);
  });

  it("never indexes private chats, edits, or the bot's own messages", () => {
    expect(
      isIndexableTelegramMessage(
        message({ chat: { id: GROUP.id, type: "private" } }),
        options,
        BOT_ID,
      ),
    ).toBe(false);
    expect(
      isIndexableTelegramMessage(message({ edit_date: 1 }), options, BOT_ID),
    ).toBe(false);
    expect(
      isIndexableTelegramMessage(
        message({ from: { first_name: "Bot", id: 777, is_bot: true } }),
        options,
        BOT_ID,
      ),
    ).toBe(false);
  });
});

describe("toContextMessage", () => {
  it("keeps sender, reply, topic, caption, and media descriptions", () => {
    expect(
      toContextMessage(
        message({
          caption: "the plan",
          document: {
            file_id: "f",
            file_name: "plan.pdf",
            file_unique_id: "u",
            mime_type: "application/pdf",
          },
          is_topic_message: true,
          message_thread_id: 42,
          photo: [{ file_id: "p", file_unique_id: "pu", height: 1, width: 1 }],
          reply_to_message: message({ message_id: 3 }),
        } as Partial<TelegramPolicyMessage>),
      ),
    ).toEqual({
      chatId: "-100123",
      chatTitle: "Ops Room",
      media: [
        { kind: "photo" },
        { kind: "document", mime_type: "application/pdf", name: "plan.pdf" },
      ],
      messageId: 5,
      replyToMessageId: 3,
      senderId: "11",
      senderName: "Ada L",
      senderUsername: "ada",
      sentAt: 1_700_000_000,
      text: "the plan",
      topicId: 42,
    });
  });

  it("ignores only a forum topic's implicit reply", () => {
    const inTopic = toContextMessage(
      message({
        is_topic_message: true,
        message_thread_id: 42,
        reply_to_message: message({ message_id: 42 }),
        text: "in a topic",
      } as Partial<TelegramPolicyMessage>),
    );
    expect(inTopic.replyToMessageId).toBeNull();
    expect(inTopic.topicId).toBe(42);

    // Outside topics message_thread_id names the reply chain's first message,
    // so a direct reply to it is still a reply.
    const inChain = toContextMessage(
      message({
        message_thread_id: 42,
        reply_to_message: message({ message_id: 42 }),
        text: "a reply",
      }),
    );
    expect(inChain.replyToMessageId).toBe(42);
    expect(inChain.topicId).toBeNull();
  });

  it("names messages sent as a chat by the chat, not Telegram's stand-in", () => {
    const result = toContextMessage(
      message({
        // Anonymous group admins arrive from GroupAnonymousBot.
        from: {
          first_name: "Group",
          id: 1_087_968_824,
          is_bot: true,
          username: "GroupAnonymousBot",
        },
        sender_chat: { id: -100123, title: "Ops Room", type: "supergroup" },
        text: "announcement",
      }),
    );
    expect(result).toEqual(
      expect.objectContaining({
        senderId: "-100123",
        senderName: "Ops Room",
        senderUsername: "",
      }),
    );
  });

  it("takes an album's caption and every item from the combined message", () => {
    const result = toContextMessage(
      message({
        media_group_id: "album-1",
        photo: [{ file_id: "p", file_unique_id: "pu", height: 1, width: 1 }],
      }),
      {
        attachments: [
          { type: "image" },
          { mimeType: "application/pdf", name: "plan.pdf", type: "file" },
        ],
        text: "launch photos",
      },
    );
    expect(result.text).toBe("launch photos");
    expect(result.media).toEqual([
      { kind: "photo" },
      { kind: "file", mime_type: "application/pdf", name: "plan.pdf" },
    ]);
  });

  it("finds content only in text, captions, or media", () => {
    expect(hasContextContent(toContextMessage(message({ text: " hi " })))).toBe(
      true,
    );
    expect(hasContextContent(toContextMessage(message({ text: "  " })))).toBe(
      false,
    );
    // A service message such as a join has neither.
    expect(hasContextContent(toContextMessage(message({})))).toBe(false);
  });
});
