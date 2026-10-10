// End-to-end ingress tests: the REAL Chat SDK + @chat-adapter/telegram stack
// talks to a fake Bot API (test/support/fake-telegram-api.ts) and a fake
// api-rs session API (ported from discordbot). Webhook-mode tests drive the
// Hono route with Telegram's secret-token header; polling-mode tests serve
// updates from the fake getUpdates.
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "bun:test";
import { createMemoryState } from "@chat-adapter/state-memory";
import {
  createTelegrambot,
  recoverRenderObligations,
  TELEGRAM_WEBHOOK_PATH,
  type Telegrambot,
  type TelegrambotApiMessage,
  type TelegrambotOptions,
  type TelegrambotSessionMessage,
} from "../src/index";
import {
  BOT_ID,
  BOT_TOKEN,
  BOT_USERNAME,
  startFakeTelegramApi,
  type FakeTelegramApi,
} from "./support/fake-telegram-api";
import {
  sampleCodexOutputLines,
  startMockCodexApi,
  type MockSessionApi,
} from "./support/mock-session-api";
import { sleep, waitFor } from "./support/net";

const SECRET = "telegrambot-webhook-secret";
const USER_ID = 5_551_234;
const OTHER_USER_ID = 5_559_999;
const GROUP_ID = -1_001_234_567_890;
const OTHER_GROUP_ID = -1_009_999_999_999;

let tg: FakeTelegramApi;
let codexApi: MockSessionApi;
let bot: Telegrambot;
let nextUpdateId = 1;
let nextMessageId = 100;

beforeAll(async () => {
  tg = await startFakeTelegramApi();
  codexApi = await startMockCodexApi();
});

beforeEach(async () => {
  tg.reset();
  codexApi.reset();
  bot = await createTestBot();
});

afterEach(async () => {
  await bot.chat.shutdown().catch(() => undefined);
});

afterAll(async () => {
  await codexApi?.close();
  await tg?.close();
});

describe("telegrambot webhook ingress", () => {
  it("rejects webhook deliveries without the secret token", async () => {
    const update = privateUpdate({ text: "hello" });
    for (const secret of [null, "wrong-secret"]) {
      const response = await postWebhook(update.body, secret);
      expect(response.status).toBe(401);
    }
    await sleep(100);
    expect(codexApi.creates).toHaveLength(0);
    expect(tg.calls.some((call) => call.method === "sendRichMessage")).toBe(
      false,
    );
  });

  it("does not expose a webhook route in polling mode", async () => {
    await bot.chat.shutdown();
    bot = await createTestBot({ mode: "polling" });
    const response = await postWebhook(privateUpdate({ text: "hi" }).body);
    expect(response.status).toBe(404);
  });

  it("answers an allowlisted DM end to end and settles 👀 → 👍", async () => {
    const update = privateUpdate({ text: "What changed in the deploy?" });
    expect((await postWebhook(update.body)).status).toBe(200);
    await waitForSettle(USER_ID, update.messageId);

    const key = `telegram:${USER_ID}`;
    expect(codexApi.creates.map((create) => create.threadKey)).toEqual([key]);
    expect(codexApi.creates[0]!.body.metadata).toEqual(
      expect.objectContaining({
        platform: "telegram",
        source: "telegrambot",
        telegram_conversation_name: "Ada Lovelace",
      }),
    );
    expect(codexApi.appends).toHaveLength(1);
    expect(sessionTexts(codexApi.appends[0]!.body.messages)).toEqual([
      "What changed in the deploy?",
    ]);
    expect(codexApi.executes).toHaveLength(1);
    const execute = codexApi.executes[0]!;
    expect(execute.body.idempotency_key).toBe(`${USER_ID}:${update.messageId}`);
    expect(execute.body.metadata).toEqual(
      expect.objectContaining({ trigger: "dm", user_id: String(USER_ID) }),
    );

    expect(tg.reactions(USER_ID, update.messageId)).toEqual(["👀", "👍"]);
    const answers = tg.botMessages(USER_ID);
    expect(answers).toHaveLength(1);
    expect(answers[0]!.text).toContain("Executed request 1.");
    // DMs are not threaded as replies.
    expect(answers[0]!.replyToMessageId).toBeUndefined();
  });

  it("claims duplicate update deliveries once and dedupes redelivered messages", async () => {
    const update = privateUpdate({ text: "run once" });
    await postWebhook(update.body);
    await postWebhook(update.body);
    await waitForSettle(USER_ID, update.messageId);

    // Same message under a new update_id (e.g. a replay after the claim TTL).
    await postWebhook({ ...update.body, update_id: nextUpdateId++ });
    await sleep(200);

    expect(codexApi.executes).toHaveLength(1);
    expect(codexApi.appends).toHaveLength(1);
    expect(tg.botMessages(USER_ID)).toHaveLength(1);
  });

  it("ignores the bot's own messages and other bots", async () => {
    await postWebhook(
      privateUpdate({ text: "echo", from: { id: BOT_ID, is_bot: true } }).body,
    );
    await postWebhook(
      groupUpdate({
        text: "bot chatter",
        from: { id: 777, is_bot: true },
        replyToBot: true,
      }).body,
    );
    await sleep(200);
    expect(codexApi.creates).toHaveLength(0);
  });

  it("is fail-closed for unlisted users, chats, and channel identities", async () => {
    const cases = [
      privateUpdate({ text: "let me in", userId: OTHER_USER_ID }),
      groupUpdate({ text: "/ask@centaur_test_bot hi", chatId: OTHER_GROUP_ID }),
      groupUpdate({
        text: "/ask@centaur_test_bot as a channel",
        senderChat: { id: -1_007_777, type: "channel", title: "News" },
      }),
      groupUpdate({
        text: "auto forwarded",
        replyToBot: true,
        automaticForward: true,
      }),
      groupUpdate({
        text: "via inline bot",
        replyToBot: true,
        viaBot: true,
      }),
    ];
    for (const update of cases) await postWebhook(update.body);
    await sleep(250);
    expect(codexApi.creates).toHaveLength(0);
    expect(
      tg.calls.filter((call) => call.method === "sendRichMessage"),
    ).toEqual([]);
  });

  it("stays inert with empty allowlists", async () => {
    await bot.chat.shutdown();
    bot = await createTestBot({ chatAllowlist: [], userAllowlist: [] });
    await postWebhook(privateUpdate({ text: "hello" }).body);
    await postWebhook(
      groupUpdate({ text: "/ask@centaur_test_bot hello" }).body,
    );
    await sleep(200);
    expect(codexApi.creates).toHaveLength(0);
  });

  it("ignores edited messages", async () => {
    const update = privateUpdate({ text: "edited", edited: true });
    await postWebhook(update.body);
    await sleep(200);
    expect(codexApi.creates).toHaveLength(0);
  });
});

describe("telegrambot group triggers", () => {
  it("ignores plain group messages and plain @mentions", async () => {
    await postWebhook(groupUpdate({ text: "just chatting" }).body);
    await postWebhook(
      groupUpdate({
        text: `@${BOT_USERNAME} are you there?`,
        entities: [
          { type: "mention", offset: 0, length: BOT_USERNAME.length + 1 },
        ],
      }).body,
    );
    await postWebhook(
      groupUpdate({ text: "/ask unaddressed", command: "/ask" }).body,
    );
    await sleep(250);
    expect(codexApi.creates).toHaveLength(0);
  });

  it("runs /ask@bot with the command stripped and replies to the trigger", async () => {
    const command = `/ask@${BOT_USERNAME}`;
    const update = groupUpdate({
      text: `${command} summarize the incident`,
      command,
      topicId: 42,
    });
    await postWebhook(update.body);
    await waitForSettle(GROUP_ID, update.messageId);

    const key = `telegram:${GROUP_ID}:42`;
    expect(codexApi.executes.map((execute) => execute.threadKey)).toEqual([
      key,
    ]);
    expect(sessionTexts(codexApi.appends[0]!.body.messages)).toEqual([
      "summarize the incident",
    ]);
    expect(codexApi.executes[0]!.body.metadata).toEqual(
      expect.objectContaining({ trigger: "command" }),
    );
    expect(codexApi.creates[0]!.body.metadata).toEqual(
      expect.objectContaining({ telegram_conversation_name: "Ops Room" }),
    );
    const answer = tg.botMessages(GROUP_ID)[0]!;
    expect(answer.text).toContain("Executed request 1.");
    expect(answer.replyToMessageId).toBe(update.messageId);
    expect(answer.threadId).toBe(42);
  });

  it("continues on a reply to the bot's answer", async () => {
    const command = `/ask@${BOT_USERNAME}`;
    const first = groupUpdate({ text: `${command} first`, command });
    await postWebhook(first.body);
    await waitForSettle(GROUP_ID, first.messageId);
    const answer = tg.botMessages(GROUP_ID)[0]!;

    const followUp = groupUpdate({
      text: "and then?",
      replyToBot: true,
      replyToMessageId: answer.messageId,
    });
    await postWebhook(followUp.body);
    await waitForSettle(GROUP_ID, followUp.messageId);

    expect(codexApi.executes).toHaveLength(2);
    expect(new Set(codexApi.executes.map((e) => e.threadKey))).toEqual(
      new Set([`telegram:${GROUP_ID}`]),
    );
    expect(codexApi.executes[1]!.body.metadata).toEqual(
      expect.objectContaining({ trigger: "reply" }),
    );
    // The session already holds the bot's answer, so it is not re-quoted.
    expect(sessionTexts(codexApi.appends[1]!.body.messages)).toEqual([
      "and then?",
    ]);
  });

  it("quotes another member's message when /ask@bot is sent as a reply", async () => {
    const command = `/ask@${BOT_USERNAME}`;
    const update = groupUpdate({
      text: `${command} what does this mean?`,
      command,
      replyTo: {
        chat: { id: GROUP_ID, title: "Ops Room", type: "supergroup" },
        date: Math.floor(Date.now() / 1000),
        from: {
          first_name: "Grace",
          id: OTHER_USER_ID,
          is_bot: false,
          username: "grace",
        },
        message_id: 77,
        text: "p99 regressed after the cache flip",
      },
    });
    await postWebhook(update.body);
    await waitForSettle(GROUP_ID, update.messageId);
    expect(sessionTexts(codexApi.appends[0]!.body.messages)[0]).toBe(
      "In reply to grace:\n> p99 regressed after the cache flip\n\nwhat does this mean?",
    );
  });

  it("answers /help and an empty /ask with usage instead of a run", async () => {
    const help = groupUpdate({
      text: `/help@${BOT_USERNAME}`,
      command: `/help@${BOT_USERNAME}`,
    });
    const empty = privateUpdate({ text: "/ask", command: "/ask" });
    await postWebhook(help.body);
    await postWebhook(empty.body);
    await waitFor(() => tg.botMessages(GROUP_ID).length === 1);
    await waitFor(() => tg.botMessages(USER_ID).length === 1);
    expect(tg.botMessages(GROUP_ID)[0]!.text).toContain("/ask@<bot>");
    expect(codexApi.creates).toHaveLength(0);
  });

  it("appends a trigger that arrives mid-run as context without a second run", async () => {
    codexApi.autoRespond = false;
    const first = privateUpdate({ text: "long task" });
    await postWebhook(first.body);
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.hasStream(`telegram:${USER_ID}`));

    const second = privateUpdate({ text: "also consider the logs" });
    await postWebhook(second.body);
    await waitFor(() => codexApi.appends.length === 2);
    await waitFor(() => tg.reactions(USER_ID, second.messageId).includes("✍"));
    expect(codexApi.executes).toHaveLength(1);

    codexApi.emitOutputLines(
      `telegram:${USER_ID}`,
      sampleCodexOutputLines("Done."),
      "exe-1",
    );
    await waitForSettle(USER_ID, first.messageId);
    expect(codexApi.executes).toHaveLength(1);
  });
});

describe("telegrambot session API failures and render outcomes", () => {
  it("retries a transient create failure without a user-visible error", async () => {
    codexApi.failCreates = 1;
    const update = privateUpdate({ text: "retry me" });
    await postWebhook(update.body);
    await waitForSettle(USER_ID, update.messageId, "👍", 8000);
    expect(codexApi.creates).toHaveLength(2);
    expect(codexApi.executes).toHaveLength(1);
    expect(tg.botMessages(USER_ID)[0]!.text).toContain("Executed request 1.");
  }, 15000);

  it("surfaces a permanent create failure and settles 👎", async () => {
    codexApi.failCreates = 1;
    codexApi.createFailureStatus = 400;
    const update = privateUpdate({ text: "bad request" });
    await postWebhook(update.body);
    await waitForSettle(USER_ID, update.messageId, "👎");
    expect(codexApi.executes).toHaveLength(0);
    const text = tg
      .botMessages(USER_ID)
      .map((m) => m.text)
      .join("\n");
    expect(text).toContain("Centaur session create session failed: 400");
    expect(text).not.toContain("create failed");
  });

  it("retries a retryable execute failure inside the render stream", async () => {
    codexApi.failNextExecute = true;
    const update = privateUpdate({ text: "execute flake" });
    await postWebhook(update.body);
    await waitForSettle(USER_ID, update.messageId);
    expect(codexApi.executes).toHaveLength(2);
    expect(
      tg
        .botMessages(USER_ID)
        .filter((m) => m.text.includes("Executed request")),
    ).toHaveLength(1);
  });

  it("reuses the accepted execution when the execute response is lost", async () => {
    codexApi.failNextExecuteAfterAccept = true;
    const update = privateUpdate({ text: "lost response" });
    await postWebhook(update.body);
    await waitForSettle(USER_ID, update.messageId);
    expect(codexApi.executes).toHaveLength(2);
    expect(
      new Set(codexApi.executes.map((e) => e.body.idempotency_key)).size,
    ).toBe(1);
    expect(tg.botMessages(USER_ID)).toHaveLength(1);
  });

  it("renders a failed turn as visible text and settles 👎", async () => {
    codexApi.autoRespond = false;
    const update = privateUpdate({ text: "fail please" });
    await postWebhook(update.body);
    await waitFor(() => codexApi.hasStream(`telegram:${USER_ID}`));
    codexApi.emitOutputLine(
      `telegram:${USER_ID}`,
      JSON.stringify({
        type: "turn.failed",
        error: { message: "Upstream exploded" },
      }),
    );
    await waitForSettle(USER_ID, update.messageId, "👎");
    expect(
      tg
        .botMessages(USER_ID)
        .map((m) => m.text)
        .join("\n"),
    ).toContain("Upstream exploded");
  });

  it("renders a completion with no final text as a visible fallback", async () => {
    codexApi.autoRespond = false;
    const update = privateUpdate({ text: "quiet run" });
    await postWebhook(update.body);
    await waitFor(() => codexApi.hasStream(`telegram:${USER_ID}`));
    codexApi.emitSessionEvent(
      `telegram:${USER_ID}`,
      "session.execution_completed",
      { execution_id: "exe-1", status: "completed" },
    );
    await waitForSettle(USER_ID, update.messageId);
    expect(
      tg
        .botMessages(USER_ID)
        .map((m) => m.text)
        .join("\n"),
    ).toContain("Execution completed, but no final text was captured.");
  });

  it("waits for visible text when the answer streams in as a bare heading marker", async () => {
    codexApi.autoRespond = false;
    const update = privateUpdate({ text: "report" });
    await postWebhook(update.body);
    await waitFor(() => codexApi.hasStream(`telegram:${USER_ID}`));
    codexApi.emitOutputLines(
      `telegram:${USER_ID}`,
      sampleCodexOutputLines(["##", " Progress Report\n\n", "- one item done."]),
      "exe-1",
    );
    await waitForSettle(USER_ID, update.messageId);
    expect(tg.reactions(USER_ID, update.messageId).at(-1)).toBe("👍");
    const answers = tg.botMessages(USER_ID);
    expect(answers).toHaveLength(1);
    expect(answers[0]!.text).toContain("Progress Report");
    expect(answers[0]!.text).toContain("one item done.");
  });

  it("splits long answers across messages", async () => {
    codexApi.autoRespond = false;
    const update = privateUpdate({ text: "long answer" });
    await postWebhook(update.body);
    await waitFor(() => codexApi.hasStream(`telegram:${USER_ID}`));
    const paragraph = `${"word ".repeat(199)}end.\n\n`;
    codexApi.emitOutputLines(
      `telegram:${USER_ID}`,
      sampleCodexOutputLines(paragraph.repeat(8)),
      "exe-1",
    );
    await waitForSettle(USER_ID, update.messageId);
    const answers = tg.botMessages(USER_ID);
    expect(answers.length).toBeGreaterThan(1);
    for (const answer of answers) {
      expect(answer.text.length).toBeLessThanOrEqual(3500);
    }
    expect(
      answers
        .map((a) => a.text)
        .join(" ")
        .split("end.").length - 1,
    ).toBe(8);
  });

  it("recovers an unfinished render obligation after a restart", async () => {
    codexApi.autoRespond = false;
    const state = createMemoryState();
    await state.connect();
    await bot.chat.shutdown();
    bot = await createTestBot({ state });
    const key = `telegram:${USER_ID}`;
    const message: TelegrambotApiMessage = {
      attachments: [],
      author: {
        fullName: "Ada Lovelace",
        isBot: false,
        isMe: false,
        userId: String(USER_ID),
        userName: "ada",
      },
      id: `${USER_ID}:4242`,
      isMention: true,
      text: "recover me",
      threadId: key,
      timestamp: new Date().toISOString(),
    };
    await bot.chat.thread(key).setState({
      activeExecution: true,
      activeExecutionStartedAt: Date.now(),
      renderObligation: { afterEventId: 0, executionId: "exe-r", message },
    });
    await state.appendToList("telegrambot:render:index", key);
    codexApi.emitOutputLines(
      key,
      sampleCodexOutputLines("Recovered."),
      "exe-r",
    );

    expect(await recoverRenderObligations(bot.chat, state, testOptions())).toBe(
      0,
    );
    expect(
      tg
        .botMessages(USER_ID)
        .map((m) => m.text)
        .join("\n"),
    ).toContain("Recovered.");
    expect(tg.reactions(USER_ID, 4242)).toEqual(["👀", "👍"]);
    expect((await bot.chat.thread(key).state)?.renderObligation).toBeNull();
    expect(codexApi.executes).toHaveLength(0);
  });
});

describe("telegrambot polling ingress", () => {
  it("processes getUpdates and advances the offset", async () => {
    await bot.chat.shutdown();
    bot = await createTestBot({ mode: "polling" });
    const update = privateUpdate({ text: "polled hello" });
    tg.enqueueUpdate(update.body);
    await waitForSettle(USER_ID, update.messageId);
    await waitFor(() =>
      tg.getUpdatesOffsets.some((offset) => offset > update.updateId),
    );
    expect(codexApi.executes).toHaveLength(1);
    const getUpdates = tg.calls.find((call) => call.method === "getUpdates");
    expect(getUpdates?.payload.allowed_updates).toEqual(["message"]);
    expect(tg.calls.some((call) => call.method === "deleteWebhook")).toBe(true);
  });

  it("replays an update whose handoff failed and executes it once", async () => {
    await bot.chat.shutdown();
    bot = await createTestBot({ mode: "polling" });
    // Three failures outlast the in-process retry; the adapter's polling
    // checkpoint then redelivers the update with backoff.
    codexApi.failCreates = 3;
    const update = privateUpdate({ text: "survive the outage" });
    tg.enqueueUpdate(update.body);
    await waitForSettle(USER_ID, update.messageId, "👍", 15000);
    expect(codexApi.creates.length).toBeGreaterThanOrEqual(4);
    expect(codexApi.executes).toHaveLength(1);
    expect(tg.botMessages(USER_ID)).toHaveLength(1);
  }, 25000);
});

function testOptions(
  overrides: Partial<TelegrambotOptions> = {},
): TelegrambotOptions {
  return {
    apiKey: "telegrambot-test-key",
    apiUrl: codexApi.url,
    botToken: BOT_TOKEN,
    chatAllowlist: [String(GROUP_ID)],
    mode: "webhook",
    pollTimeoutSeconds: 1,
    recoverRenderObligationsOnStart: false,
    state: createMemoryState(),
    telegramApiUrl: tg.url,
    userAllowlist: [String(USER_ID)],
    webhookSecretToken: SECRET,
    ...overrides,
  };
}

async function createTestBot(
  overrides: Partial<TelegrambotOptions> = {},
): Promise<Telegrambot> {
  const created = createTelegrambot(testOptions(overrides));
  await created.chat.initialize();
  return created;
}

async function postWebhook(
  body: Record<string, unknown>,
  secret: string | null = SECRET,
): Promise<Response> {
  return bot.app.request(TELEGRAM_WEBHOOK_PATH, {
    body: JSON.stringify(body),
    headers: {
      "content-type": "application/json",
      ...(secret ? { "x-telegram-bot-api-secret-token": secret } : {}),
    },
    method: "POST",
  });
}

type BuiltUpdate = {
  body: Record<string, unknown>;
  messageId: number;
  updateId: number;
};

type MessageInput = {
  command?: string;
  edited?: boolean;
  entities?: Record<string, unknown>[];
  from?: { id: number; is_bot: boolean };
  text: string;
};

function privateUpdate(input: MessageInput & { userId?: number }): BuiltUpdate {
  const userId = input.userId ?? USER_ID;
  return buildUpdate(input, {
    chat: {
      first_name: "Ada",
      id: userId,
      last_name: "Lovelace",
      type: "private",
    },
    from: input.from
      ? { first_name: "X", ...input.from }
      : {
          first_name: "Ada",
          id: userId,
          is_bot: false,
          last_name: "Lovelace",
          username: "ada",
        },
  });
}

function groupUpdate(
  input: MessageInput & {
    automaticForward?: boolean;
    chatId?: number;
    replyTo?: Record<string, unknown>;
    replyToBot?: boolean;
    replyToMessageId?: number;
    senderChat?: Record<string, unknown>;
    topicId?: number;
    viaBot?: boolean;
  },
): BuiltUpdate {
  const replyTo =
    input.replyTo ??
    (input.replyToBot
      ? {
          chat: { id: input.chatId ?? GROUP_ID, type: "supergroup" },
          date: Math.floor(Date.now() / 1000),
          from: {
            first_name: "Centaur",
            id: BOT_ID,
            is_bot: true,
            username: BOT_USERNAME,
          },
          message_id: input.replyToMessageId ?? 1,
          text: "an earlier answer",
        }
      : undefined);
  return buildUpdate(input, {
    chat: {
      id: input.chatId ?? GROUP_ID,
      title: "Ops Room",
      type: "supergroup",
    },
    from: input.from
      ? { first_name: "X", ...input.from }
      : { first_name: "Ada", id: USER_ID, is_bot: false, username: "ada" },
    ...(input.topicId
      ? { message_thread_id: input.topicId, is_topic_message: true }
      : {}),
    ...(replyTo ? { reply_to_message: replyTo } : {}),
    ...(input.senderChat ? { sender_chat: input.senderChat } : {}),
    ...(input.automaticForward ? { is_automatic_forward: true } : {}),
    ...(input.viaBot
      ? { via_bot: { first_name: "Inline", id: 888, is_bot: true } }
      : {}),
  });
}

function buildUpdate(
  input: MessageInput,
  fields: Record<string, unknown>,
): BuiltUpdate {
  const messageId = nextMessageId++;
  const updateId = nextUpdateId++;
  const entities =
    input.entities ??
    (input.command
      ? [{ length: input.command.length, offset: 0, type: "bot_command" }]
      : undefined);
  const message = {
    date: Math.floor(Date.now() / 1000),
    message_id: messageId,
    text: input.text,
    ...(entities ? { entities } : {}),
    ...(input.edited ? { edit_date: Math.floor(Date.now() / 1000) } : {}),
    ...fields,
  };
  return {
    body: { message, update_id: updateId },
    messageId,
    updateId,
  };
}

async function waitForSettle(
  chatId: number,
  messageId: number,
  emoji: "👍" | "👎" = "👍",
  timeoutMs = 5000,
): Promise<void> {
  await waitFor(
    () => tg.reactions(chatId, messageId).includes(emoji),
    timeoutMs,
  );
}

function sessionTexts(messages: TelegrambotSessionMessage[]): string[] {
  return messages.flatMap((message) =>
    message.parts.flatMap((part) =>
      part &&
      typeof part === "object" &&
      !Array.isArray(part) &&
      part.type === "text" &&
      typeof part.text === "string"
        ? [part.text]
        : [],
    ),
  );
}
