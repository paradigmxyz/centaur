import { describe, expect, it } from "bun:test";
import type { TelegramApi } from "../src/telegram-api";
import {
  codexAttachmentInput,
  executeSessionTurn,
  forwardToSessionApi,
  isContentlessApiMessage,
  isRetryableSessionApiError,
  isSteeringDeliveredEvent,
  isSteeringFailedEvent,
  MAX_TELEGRAM_ATTACHMENT_BYTES,
  MAX_TELEGRAM_ATTACHMENTS,
  openSessionEventStream,
  serializeTelegramAttachmentList,
  serializeTelegramAttachments,
  serializeTelegramMessage,
  SESSION_STEERING_DELIVERED_EVENT,
  SESSION_STEERING_FAILED_EVENT,
  SessionApiError,
  steeringDeliveredMessageIds,
  steeringFailedError,
  telegramClientMessageId,
  toCodexInputLines,
} from "../src/session-api";
import type {
  ForwardSessionInput,
  TelegramApiMessage,
  TelegramAttachmentCandidate,
} from "../src/session-api";
import type {
  TelegramMessage,
  TelegrambotFetch,
  TelegrambotOptions,
  TelegrambotRendererSource,
} from "../src/types";

type JsonRecord = Record<string, unknown>;

const THREAD_KEY = "telegram:chat:-1001";

function telegramMessage(
  overrides: Partial<TelegramMessage> = {},
): TelegramMessage {
  return {
    message_id: 42,
    from: {
      id: 777,
      is_bot: false,
      first_name: "Alice",
      last_name: "Liddell",
      username: "alice",
    },
    chat: { id: -1001, type: "supergroup", title: "ops" },
    date: 1_767_225_600,
    text: "hello",
    ...overrides,
  };
}

function apiMessage(
  overrides: Partial<TelegramApiMessage> = {},
): TelegramApiMessage {
  return {
    attachments: [],
    author: {
      fullName: "Alice Liddell",
      isBot: false,
      isMe: false,
      userId: "777",
      userName: "alice",
    },
    id: "telegram:-1001:42",
    isMention: true,
    raw: {},
    text: "hello",
    threadId: THREAD_KEY,
    timestamp: "2026-01-01T00:00:00.000Z",
    ...overrides,
  };
}

function options(fetchFn: TelegrambotFetch): TelegrambotOptions {
  return {
    apiUrl: "http://api.test",
    botToken: "token",
    fetch: fetchFn,
  };
}

function forwardInput(
  overrides: Partial<ForwardSessionInput> = {},
): ForwardSessionInput {
  return {
    afterEventId: 0,
    create: {
      conversationName: "ops",
      chatType: "supergroup",
      userId: "777",
      messageThreadId: 9,
    },
    executeMessage: apiMessage(),
    messages: [apiMessage()],
    onEventId: () => undefined,
    openStream: false,
    threadKey: THREAD_KEY,
    ...overrides,
  };
}

type RecordedRequest = { url: string; body?: JsonRecord };

function recorderApi(): {
  fetchFn: TelegrambotFetch;
  creates: JsonRecord[];
  appends: JsonRecord[];
  executes: JsonRecord[];
  requests: RecordedRequest[];
} {
  const creates: JsonRecord[] = [];
  const appends: JsonRecord[] = [];
  const executes: JsonRecord[] = [];
  const requests: RecordedRequest[] = [];
  const fetchFn: TelegrambotFetch = async (input, init) => {
    const url = String(input);
    const body = init?.body
      ? (JSON.parse(String(init.body)) as JsonRecord)
      : undefined;
    requests.push({ url, body });
    if (url.endsWith("/execute")) {
      executes.push(body ?? {});
      return Response.json({
        execution_id: "exec-1",
        ok: true,
        status: "running",
        thread_key: THREAD_KEY,
      });
    }
    if (url.endsWith("/messages")) {
      appends.push(body ?? {});
      return Response.json({ ok: true, message_ids: ["msg_1", "msg_2"] });
    }
    creates.push(body ?? {});
    return Response.json({ ok: true });
  };
  return { fetchFn, creates, appends, executes, requests };
}

describe("isRetryableSessionApiError", () => {
  it("respects the SessionApiError retryable flag", () => {
    const retryable = new SessionApiError({
      action: "create session",
      body: "",
      retryable: true,
      status: 503,
      statusText: "Service Unavailable",
    });
    const fatal = new SessionApiError({
      action: "create session",
      body: "",
      retryable: false,
      status: 400,
      statusText: "Bad Request",
    });
    expect(isRetryableSessionApiError(retryable)).toBe(true);
    expect(isRetryableSessionApiError(fatal)).toBe(false);
  });

  it("treats AbortError as retryable", () => {
    const error = new Error("aborted");
    error.name = "AbortError";
    expect(isRetryableSessionApiError(error)).toBe(true);
  });

  it("treats TypeError as retryable (fetch network failures)", () => {
    expect(isRetryableSessionApiError(new TypeError("fetch failed"))).toBe(
      true,
    );
  });

  it("does not retry generic errors or non-errors", () => {
    expect(isRetryableSessionApiError(new Error("boom"))).toBe(false);
    expect(isRetryableSessionApiError("boom")).toBe(false);
    expect(isRetryableSessionApiError(undefined)).toBe(false);
  });

  it("classifies retryable statuses from real API failures", async () => {
    const codes: Array<[number, boolean]> = [
      [400, false],
      [404, false],
      [408, true],
      [409, false],
      [425, true],
      [429, true],
      [500, true],
      [503, true],
    ];
    for (const [status, retryable] of codes) {
      const fetchFn: TelegrambotFetch = async () =>
        new Response("nope", { status });
      let caught: unknown;
      try {
        await forwardToSessionApi(options(fetchFn), forwardInput());
      } catch (error) {
        caught = error;
      }
      expect(caught).toBeInstanceOf(SessionApiError);
      expect((caught as SessionApiError).retryable).toBe(retryable);
    }
  });
});

describe("serializeTelegramMessage", () => {
  it("derives the stable client message id from chat and message ids", () => {
    expect(telegramClientMessageId(telegramMessage())).toBe(
      "telegram:-1001:42",
    );
    const message = serializeTelegramMessage(telegramMessage(), THREAD_KEY);
    expect(message.id).toBe("telegram:-1001:42");
  });

  it("maps author metadata from the from field", () => {
    const message = serializeTelegramMessage(telegramMessage(), THREAD_KEY);
    expect(message.author).toEqual({
      fullName: "Alice Liddell",
      isBot: false,
      isMe: false,
      userId: "777",
      userName: "alice",
    });
    expect(message.threadId).toBe(THREAD_KEY);
    expect(message.timestamp).toBe(new Date(1_767_225_600_000).toISOString());
  });

  it("falls back to the caption when text is absent", () => {
    const message = serializeTelegramMessage(
      telegramMessage({ text: undefined, caption: "look at this" }),
      THREAD_KEY,
    );
    expect(message.text).toBe("look at this");
  });

  it("falls back to the user id when the user has no username", () => {
    const message = serializeTelegramMessage(
      telegramMessage({
        from: { id: 777, is_bot: false, first_name: "Alice" },
      }),
      THREAD_KEY,
    );
    expect(message.author.userName).toBe("777");
    expect(message.author.fullName).toBe("Alice");
  });
});

describe("isContentlessApiMessage", () => {
  it("is true for empty text with no attachments (sticker/poll/service)", () => {
    expect(isContentlessApiMessage(apiMessage({ text: "" }))).toBe(true);
    expect(isContentlessApiMessage(apiMessage({ text: "  \n " }))).toBe(true);
  });

  it("is false when there is text or an attachment", () => {
    expect(isContentlessApiMessage(apiMessage({ text: "do the thing" }))).toBe(
      false,
    );
    expect(
      isContentlessApiMessage(
        apiMessage({ text: "", attachments: [{ type: "image" }] }),
      ),
    ).toBe(false);
  });
});

describe("forwardToSessionApi", () => {
  it("sends the full principal metadata on every create", async () => {
    const { fetchFn, creates } = recorderApi();
    await forwardToSessionApi(options(fetchFn), forwardInput());
    expect(creates[0]?.metadata).toEqual({
      source: "telegrambot",
      platform: "telegram",
      thread_id: THREAD_KEY,
      telegram_conversation_name: "ops",
      telegram_chat_type: "supergroup",
      user_id: "777",
      message_thread_id: 9,
    });
    expect(creates[0]?.harness_type).toBe("codex");
  });

  it("omits message_thread_id when absent and blank conversation names", async () => {
    const { fetchFn, creates } = recorderApi();
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput({
        create: { conversationName: "  ", chatType: "private", userId: "777" },
      }),
    );
    const metadata = creates[0]?.metadata as JsonRecord;
    expect("message_thread_id" in metadata).toBe(false);
    expect("telegram_conversation_name" in metadata).toBe(false);
    expect(metadata.telegram_chat_type).toBe("private");
  });

  it("appends with the stable client_message_id and user role", async () => {
    const { fetchFn, appends } = recorderApi();
    await forwardToSessionApi(options(fetchFn), forwardInput());
    const appended = (appends[0]?.messages as JsonRecord[])[0]!;
    expect(appended.client_message_id).toBe("telegram:-1001:42");
    expect(appended.role).toBe("user");
    expect(appended.parts).toEqual([{ type: "text", text: "hello" }]);
    expect((appended.metadata as JsonRecord).user_id).toBe("777");
  });

  it("hands the server-assigned message ids to onMessagesAppended for steering correlation", async () => {
    const { fetchFn } = recorderApi();
    let appendedIds: string[] | undefined;
    await forwardToSessionApi(options(fetchFn), forwardInput(), {
      onMessagesAppended: async (messageIds) => {
        appendedIds = messageIds;
      },
    });
    expect(appendedIds).toEqual(["msg_1", "msg_2"]);
  });

  it("skips the append call when there are no messages", async () => {
    const { fetchFn, appends } = recorderApi();
    await forwardToSessionApi(
      options(fetchFn),
      forwardInput({ messages: [], executeMessage: undefined }),
    );
    expect(appends).toHaveLength(0);
  });

  it("executes with the message id as idempotency key", async () => {
    const { fetchFn, executes } = recorderApi();
    const execution = await forwardToSessionApi(
      options(fetchFn),
      forwardInput(),
    );
    // openStream is false: no stream is returned even though execute ran.
    expect(execution).toBeNull();
    expect(executes[0]?.idempotency_key).toBe("telegram:-1001:42");
    expect(
      (executes[0]?.input_lines as string[]).length,
    ).toBeGreaterThanOrEqual(1);
  });
});

describe("executeSessionTurn", () => {
  it("returns the accepted execution", async () => {
    const { fetchFn, executes } = recorderApi();
    const execution = await executeSessionTurn(
      options(fetchFn),
      forwardInput(),
    );
    expect(execution?.execution_id).toBe("exec-1");
    expect(executes[0]?.idempotency_key).toBe("telegram:-1001:42");
  });

  it("is a no-op without an execute message", async () => {
    const { fetchFn, executes } = recorderApi();
    const execution = await executeSessionTurn(
      options(fetchFn),
      forwardInput({ executeMessage: undefined }),
    );
    expect(execution).toBeNull();
    expect(executes).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// SSE replay
// ---------------------------------------------------------------------------

function sseResponseFor(events: string): TelegrambotFetch & {
  urls: string[];
} {
  const urls: string[] = [];
  const fetchFn = (async (input: RequestInfo | URL) => {
    urls.push(String(input));
    return new Response(events, { status: 200 });
  }) as TelegrambotFetch & { urls: string[] };
  fetchFn.urls = urls;
  return fetchFn;
}

async function collect(
  stream: AsyncIterable<TelegrambotRendererSource>,
): Promise<TelegrambotRendererSource[]> {
  const items: TelegrambotRendererSource[] = [];
  for await (const item of stream) items.push(item);
  return items;
}

describe("openSessionEventStream", () => {
  const sseBody = [
    "id: 5",
    "event: session.output.line",
    'data: {"type":"item.completed"}',
    "",
    "id: 6",
    "event: session.steering_delivered",
    'data: {"execution_id":"exec-1","thread_key":"telegram:chat:-1001","message_ids":["msg_9"],"input_line_count":1}',
    "",
    "id: 7",
    "event: session.execution_completed",
    'data: {"execution_id":"exec-1"}',
    "",
  ].join("\n");

  it("requests after_event_id and execution_id and reports observed event ids", async () => {
    const fetchFn = sseResponseFor(sseBody);
    const seen: number[] = [];
    const stream = await openSessionEventStream(options(fetchFn), {
      afterEventId: 4,
      executionId: "exec-1",
      onEventId: (eventId) => seen.push(eventId),
      threadKey: THREAD_KEY,
    });
    const events = await collect(stream);

    const url = new URL(fetchFn.urls[0]!);
    expect(url.searchParams.get("after_event_id")).toBe("4");
    expect(url.searchParams.get("execution_id")).toBe("exec-1");
    expect(seen).toEqual([5, 6, 7]);
    expect(events.map((event) => (event as JsonRecord).eventKind)).toEqual([
      "session.output.line",
      "session.steering_delivered",
      "session.execution_completed",
    ]);
  });

  it("passes steering events through non-terminally with parsed data", async () => {
    const fetchFn = sseResponseFor(sseBody);
    const stream = await openSessionEventStream(options(fetchFn), {
      afterEventId: 0,
      onEventId: () => undefined,
      threadKey: THREAD_KEY,
    });
    const events = await collect(stream);
    const steering = events.find(isSteeringDeliveredEvent);
    expect(steering).toBeDefined();
    expect(steeringDeliveredMessageIds(steering!)).toEqual(["msg_9"]);
    // The stream continued past steering to the terminal completion event.
    expect(
      (events.at(-1) as JsonRecord).eventKind,
    ).toBe("session.execution_completed");
  });

  it("stops at terminal events and replays only later events on reconnect", async () => {
    const fetchFn = sseResponseFor(
      [
        "id: 3",
        "event: session.output.line",
        'data: {"type":"item.completed"}',
        "",
        "id: 4",
        "event: session.execution_failed",
        'data: {"error":"sandbox died"}',
        "",
        "id: 5",
        "event: session.output.line",
        'data: {"type":"item.completed"}',
        "",
      ].join("\n"),
    );
    let lastEventId = 0;
    const stream = await openSessionEventStream(options(fetchFn), {
      afterEventId: 0,
      onEventId: (eventId) => {
        lastEventId = eventId;
      },
      threadKey: THREAD_KEY,
    });
    const events = await collect(stream);
    expect((events.at(-1) as JsonRecord).eventKind).toBe(
      "session.execution_failed",
    );
    expect((events.at(-1) as JsonRecord).data).toEqual({
      error: "sandbox died",
    });
    // Terminal error ends iteration; event 5 is never surfaced.
    expect(events).toHaveLength(2);
    expect(lastEventId).toBe(4);

    // Reconnect resumes strictly after the last durably observed event id.
    const reconnect = await openSessionEventStream(options(fetchFn), {
      afterEventId: lastEventId,
      onEventId: () => undefined,
      threadKey: THREAD_KEY,
    });
    await collect(reconnect);
    const url = new URL(fetchFn.urls[1]!);
    expect(url.searchParams.get("after_event_id")).toBe("4");
  });

  it("treats a terminal codex output line as end of stream", async () => {
    const fetchFn = sseResponseFor(
      [
        "id: 1",
        "event: session.output.line",
        'data: {"type":"turn.completed"}',
        "",
        "id: 2",
        "event: session.output.line",
        'data: {"type":"item.completed"}',
        "",
      ].join("\n"),
    );
    const stream = await openSessionEventStream(options(fetchFn), {
      afterEventId: 0,
      onEventId: () => undefined,
      threadKey: THREAD_KEY,
    });
    const events = await collect(stream);
    expect(events).toHaveLength(1);
  });
});

describe("steering event guards", () => {
  const delivered: TelegrambotRendererSource = {
    event: SESSION_STEERING_DELIVERED_EVENT,
    eventKind: SESSION_STEERING_DELIVERED_EVENT,
    eventId: 12,
    data: {
      execution_id: "exec-1",
      thread_key: THREAD_KEY,
      message_ids: ["msg_1", "msg_2"],
      input_line_count: 2,
    },
  };
  const failed: TelegrambotRendererSource = {
    event: SESSION_STEERING_FAILED_EVENT,
    eventKind: SESSION_STEERING_FAILED_EVENT,
    eventId: 13,
    data: {
      execution_id: "exec-1",
      thread_key: THREAD_KEY,
      error: "stdin pipe closed",
    },
  };

  it("detects delivered and failed steering events", () => {
    expect(isSteeringDeliveredEvent(delivered)).toBe(true);
    expect(isSteeringFailedEvent(delivered)).toBe(false);
    expect(isSteeringFailedEvent(failed)).toBe(true);
    expect(isSteeringDeliveredEvent(failed)).toBe(false);
  });

  it("ignores mapper notifications and other session events", () => {
    const notification: TelegrambotRendererSource = {
      method: "item/started",
      params: {},
    };
    expect(isSteeringDeliveredEvent(notification)).toBe(false);
    expect(isSteeringFailedEvent(notification)).toBe(false);
    expect(
      isSteeringDeliveredEvent({
        event: "session.execution_completed",
        eventKind: "session.execution_completed",
      }),
    ).toBe(false);
  });

  it("extracts the delivered message ids and failure error", () => {
    expect(steeringDeliveredMessageIds(delivered)).toEqual(["msg_1", "msg_2"]);
    expect(steeringDeliveredMessageIds(failed)).toEqual([]);
    expect(steeringFailedError(failed)).toBe("stdin pipe closed");
    expect(steeringFailedError(delivered)).toBeUndefined();
  });

  it("tolerates malformed steering payloads", () => {
    expect(
      steeringDeliveredMessageIds({
        event: SESSION_STEERING_DELIVERED_EVENT,
        eventKind: SESSION_STEERING_DELIVERED_EVENT,
        data: { message_ids: [1, "msg_1", null] },
      }),
    ).toEqual(["msg_1"]);
    expect(
      steeringFailedError({
        event: SESSION_STEERING_FAILED_EVENT,
        eventKind: SESSION_STEERING_FAILED_EVENT,
        data: "not json",
      }),
    ).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

function fakeTelegramApi(overrides: Partial<TelegramApi> = {}): TelegramApi {
  const unexpected = (method: string) => async () => {
    throw new Error(`unexpected call: ${method}`);
  };
  return {
    getMe: unexpected("getMe") as TelegramApi["getMe"],
    getUpdates: unexpected("getUpdates") as TelegramApi["getUpdates"],
    deleteWebhook: unexpected("deleteWebhook") as TelegramApi["deleteWebhook"],
    sendMessage: unexpected("sendMessage") as TelegramApi["sendMessage"],
    editMessageText: unexpected(
      "editMessageText",
    ) as TelegramApi["editMessageText"],
    setMessageReaction: unexpected(
      "setMessageReaction",
    ) as TelegramApi["setMessageReaction"],
    sendChatAction: unexpected(
      "sendChatAction",
    ) as TelegramApi["sendChatAction"],
    getFile: unexpected("getFile") as TelegramApi["getFile"],
    downloadFile: unexpected("downloadFile") as TelegramApi["downloadFile"],
    ...overrides,
  };
}

function candidate(
  overrides: Partial<TelegramAttachmentCandidate> = {},
): TelegramAttachmentCandidate {
  return {
    fileId: "file-1",
    fileUniqueId: "uniq-1",
    mimeType: "image/jpeg",
    name: "photo.jpg",
    size: 4,
    type: "image",
    ...overrides,
  };
}

describe("serializeTelegramAttachments", () => {
  it("downloads only the largest photo size and inlines it as base64", async () => {
    const bytes = new Uint8Array([1, 2, 3, 4]);
    const requestedFileIds: string[] = [];
    const api = fakeTelegramApi({
      getFile: async (fileId) => {
        requestedFileIds.push(fileId);
        return {
          file_id: fileId,
          file_unique_id: "uniq-big",
          file_path: "photos/big.jpg",
        };
      },
      downloadFile: async () => bytes,
    });
    const message = telegramMessage({
      text: undefined,
      caption: "pic",
      photo: [
        // Deliberately unsorted: selection must not trust ordering.
        {
          file_id: "big",
          file_unique_id: "uniq-big",
          width: 800,
          height: 600,
        },
        {
          file_id: "small",
          file_unique_id: "uniq-small",
          width: 90,
          height: 67,
        },
      ],
    });

    const attachments = await serializeTelegramAttachments(api, message);

    expect(requestedFileIds).toEqual(["big"]);
    expect(attachments).toHaveLength(1);
    expect(attachments[0]?.type).toBe("image");
    expect(attachments[0]?.mimeType).toBe("image/jpeg");
    expect(attachments[0]?.dataBase64).toBe(
      Buffer.from(bytes).toString("base64"),
    );
    expect(attachments[0]?.fetchError).toBeUndefined();
  });

  it("serializes documents with their declared name and mime type", async () => {
    const api = fakeTelegramApi({
      getFile: async (fileId) => ({
        file_id: fileId,
        file_unique_id: "uniq-doc",
        file_path: "documents/report.pdf",
      }),
      downloadFile: async (filePath) => {
        expect(filePath).toBe("documents/report.pdf");
        return new Uint8Array([9, 9]);
      },
    });
    const message = telegramMessage({
      text: undefined,
      caption: "report",
      document: {
        file_id: "doc-1",
        file_unique_id: "uniq-doc",
        file_name: "report.pdf",
        mime_type: "application/pdf",
        file_size: 2,
      },
    });

    const attachments = await serializeTelegramAttachments(api, message);

    expect(attachments).toHaveLength(1);
    expect(attachments[0]).toMatchObject({
      type: "file",
      name: "report.pdf",
      mimeType: "application/pdf",
    });
    expect(attachments[0]?.dataBase64).toBe(
      Buffer.from([9, 9]).toString("base64"),
    );
  });

  it("skips the download entirely when the declared size exceeds the cap", async () => {
    const api = fakeTelegramApi(); // any API call throws "unexpected call"
    const message = telegramMessage({
      document: {
        file_id: "doc-1",
        file_unique_id: "uniq-doc",
        file_name: "huge.bin",
        file_size: MAX_TELEGRAM_ATTACHMENT_BYTES + 1,
      },
    });

    const attachments = await serializeTelegramAttachments(api, message);

    expect(attachments[0]?.dataBase64).toBeUndefined();
    expect(attachments[0]?.fetchError).toContain("too large");
  });

  it("re-checks the actual byte count when size metadata is absent", async () => {
    const api = fakeTelegramApi({
      getFile: async () => ({
        file_id: "doc-1",
        file_unique_id: "uniq-doc",
        file_path: "documents/huge.bin",
      }),
      downloadFile: async () =>
        new Uint8Array(MAX_TELEGRAM_ATTACHMENT_BYTES + 1),
    });
    const message = telegramMessage({
      document: {
        file_id: "doc-1",
        file_unique_id: "uniq-doc",
        file_name: "huge.bin",
      },
    });

    const attachments = await serializeTelegramAttachments(api, message);

    expect(attachments[0]?.dataBase64).toBeUndefined();
    expect(attachments[0]?.fetchError).toContain("too large");
  });

  it("records a fetchError instead of throwing when the download fails", async () => {
    const api = fakeTelegramApi({
      getFile: async () => {
        throw new Error("telegram getFile failed: 400 file is too big");
      },
    });
    const message = telegramMessage({
      photo: [
        { file_id: "p", file_unique_id: "uniq-p", width: 10, height: 10 },
      ],
    });

    const attachments = await serializeTelegramAttachments(api, message);

    expect(attachments[0]?.dataBase64).toBeUndefined();
    expect(attachments[0]?.fetchError).toContain("file is too big");
  });

  it("records a fetchError when getFile returns no file_path", async () => {
    const api = fakeTelegramApi({
      getFile: async () => ({ file_id: "p", file_unique_id: "uniq-p" }),
    });
    const message = telegramMessage({
      photo: [
        { file_id: "p", file_unique_id: "uniq-p", width: 10, height: 10 },
      ],
    });

    const attachments = await serializeTelegramAttachments(api, message);

    expect(attachments[0]?.fetchError).toContain("no file_path");
  });

  it("returns no attachments for a plain text message", async () => {
    const attachments = await serializeTelegramAttachments(
      fakeTelegramApi(),
      telegramMessage(),
    );
    expect(attachments).toEqual([]);
  });

  it("caps downloads at MAX_TELEGRAM_ATTACHMENTS and notes the skipped rest", async () => {
    let downloads = 0;
    const api = fakeTelegramApi({
      getFile: async (fileId) => ({
        file_id: fileId,
        file_unique_id: `uniq-${fileId}`,
        file_path: `files/${fileId}`,
      }),
      downloadFile: async () => {
        downloads += 1;
        return new Uint8Array([1]);
      },
    });
    const candidates = Array.from({ length: MAX_TELEGRAM_ATTACHMENTS + 2 }).map(
      (_, index) =>
        candidate({ fileId: `file-${index}`, fileUniqueId: `uniq-${index}` }),
    );

    const attachments = await serializeTelegramAttachmentList(api, candidates);

    expect(attachments).toHaveLength(MAX_TELEGRAM_ATTACHMENTS + 2);
    expect(downloads).toBe(MAX_TELEGRAM_ATTACHMENTS);
    const skipped = attachments.slice(MAX_TELEGRAM_ATTACHMENTS);
    expect(skipped.every((a) => a.dataBase64 === undefined)).toBe(true);
    expect(skipped.every((a) => a.fetchError?.includes("cap"))).toBe(true);
  });
});

describe("codexAttachmentInput", () => {
  it("inlines an image with bytes as a data: URL", () => {
    const out = codexAttachmentInput({
      type: "image",
      mimeType: "image/jpeg",
      dataBase64: "QUJD",
      name: "photo.jpg",
    }) as JsonRecord;
    expect(out.type).toBe("image");
    expect(out.url).toBe("data:image/jpeg;base64,QUJD");
  });

  it("references a staged attachment id instead of inlining", () => {
    const out = codexAttachmentInput(
      { type: "image", mimeType: "image/jpeg", dataBase64: "QUJD" },
      "att-m1-1",
    ) as JsonRecord;
    expect(out).toMatchObject({
      type: "attachment",
      stagedAttachmentId: "att-m1-1",
    });
    expect(out.dataBase64).toBeUndefined();
    expect(out.url).toBeUndefined();
  });

  it("describes byte-less attachments (fetch failures) as text", () => {
    const out = codexAttachmentInput({
      type: "file",
      name: "huge.bin",
      fetchError: "attachment too large to inline",
    }) as JsonRecord;
    expect(out.type).toBe("text");
    expect(out.text).toContain("huge.bin");
    expect(out.text).toContain("too large");
  });
});

describe("toCodexInputLines", () => {
  it("inlines a small image in a single user line as a data: URL", () => {
    const message = apiMessage({
      attachments: [
        {
          type: "image",
          mimeType: "image/jpeg",
          dataBase64: "QUJD",
          name: "photo.jpg",
        },
      ],
    });

    const lines = toCodexInputLines(message, message.threadId);

    expect(lines).toHaveLength(1);
    const parsed = JSON.parse(lines[0]!) as {
      thread_key: string;
      trace_metadata: JsonRecord;
      message: { content: JsonRecord[] };
    };
    expect(parsed.thread_key).toBe(THREAD_KEY);
    expect(parsed.trace_metadata.source).toBe("telegrambot");
    expect(parsed.trace_metadata.platform).toBe("telegram");
    const image = parsed.message.content.find((part) => part.type === "image");
    expect(image?.url).toBe("data:image/jpeg;base64,QUJD");
  });

  it("stages a large image as chunk lines plus a referencing user line", () => {
    const dataBase64 = Buffer.alloc(700 * 1024, 1).toString("base64");
    const message = apiMessage({
      attachments: [
        {
          type: "image",
          mimeType: "image/jpeg",
          dataBase64,
          name: "photo.jpg",
        },
      ],
    });

    const lines = toCodexInputLines(message, message.threadId);

    expect(lines.length).toBeGreaterThan(1);
    const chunks = lines.slice(0, -1).map((line) => JSON.parse(line));
    expect(chunks.every((c) => c.type === "attachment.chunk")).toBe(true);
    expect(chunks.at(-1).final).toBe(true);
    // The chunks must reassemble to the original base64 payload.
    expect(chunks.map((c) => c.dataBase64).join("")).toBe(dataBase64);

    const lastLine = lines[lines.length - 1]!;
    const content = JSON.parse(lastLine).message.content as JsonRecord[];
    const ref = content.find((part) => part.type === "attachment");
    expect(ref?.stagedAttachmentId).toBe(chunks[0].attachmentId);
    // The huge payload must NOT also be inlined in the user line.
    expect(lastLine.length).toBeLessThan(dataBase64.length);
  });

  it("emits a synthetic continue turn for empty content", () => {
    const lines = toCodexInputLines(apiMessage({ text: "  " }), THREAD_KEY);
    const content = JSON.parse(lines[0]!).message.content as JsonRecord[];
    expect(content).toEqual([{ type: "text", text: "continue" }]);
  });
});
