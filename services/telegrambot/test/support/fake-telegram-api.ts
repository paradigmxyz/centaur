// A minimal Telegram Bot API over node:http that the REAL @chat-adapter/telegram
// adapter talks to (via its apiUrl override), so the full Chat SDK pipeline —
// webhook verification, update claims, polling checkpoints, locks, routing —
// runs exactly as in production.
import {
  createServer,
  type IncomingMessage,
  type Server as HttpServer,
  type ServerResponse,
} from "node:http";
import { markdownToPlainText } from "chat";
import { availablePort, closeServer, listen } from "./net";

export const BOT_TOKEN = "123456:telegrambot-emulate-token";
export const BOT_ID = 900_000_001;
export const BOT_USERNAME = "centaur_test_bot";

export type FakeTelegramCall = {
  method: string;
  payload: Record<string, unknown>;
};

export type FakeTelegramMessage = {
  chatId: number;
  messageId: number;
  replyToMessageId?: number;
  text: string;
  threadId?: number;
};

export type FakeTelegramApi = {
  calls: FakeTelegramCall[];
  close(): Promise<void>;
  /** Bot-authored messages, in send order, with their latest edited text. */
  botMessages(chatId: number): FakeTelegramMessage[];
  /** Updates served by the next getUpdates call(s), honoring `offset`. */
  enqueueUpdate(update: Record<string, unknown>): void;
  failMethods: Map<string, number>;
  files: Map<string, Buffer>;
  getUpdatesOffsets: number[];
  reactions(chatId: number, messageId: number): string[];
  reset(): void;
  url: string;
};

export async function startFakeTelegramApi(): Promise<FakeTelegramApi> {
  const calls: FakeTelegramCall[] = [];
  const messages = new Map<string, FakeTelegramMessage>();
  const updates: Record<string, unknown>[] = [];
  const getUpdatesOffsets: number[] = [];
  const failMethods = new Map<string, number>();
  const files = new Map<string, Buffer>();
  let nextMessageId = 5_000;
  const port = await availablePort(4263);
  const url = `http://127.0.0.1:${port}`;

  const sendMessage = (payload: Record<string, unknown>) => {
    const chatId = Number(payload.chat_id);
    const messageId = ++nextMessageId;
    const rich = payload.rich_message as { markdown?: string } | undefined;
    const reply = payload.reply_parameters as
      { message_id?: number } | undefined;
    const threadId =
      typeof payload.message_thread_id === "number"
        ? payload.message_thread_id
        : undefined;
    const message: FakeTelegramMessage = {
      chatId,
      messageId,
      replyToMessageId: reply?.message_id,
      text: String(rich?.markdown ?? payload.text ?? ""),
      threadId,
    };
    messages.set(`${chatId}:${messageId}`, message);
    return {
      chat: { id: chatId, type: chatId > 0 ? "private" : "supergroup" },
      date: Math.floor(Date.now() / 1000),
      from: {
        first_name: "Centaur",
        id: BOT_ID,
        is_bot: true,
        username: BOT_USERNAME,
      },
      message_id: messageId,
      ...(threadId ? { message_thread_id: threadId } : {}),
      text: message.text,
    };
  };

  const handle = async (
    req: IncomingMessage,
    res: ServerResponse,
  ): Promise<void> => {
    const path = new URL(req.url ?? "/", url).pathname;
    const fileMatch = /^\/file\/bot([^/]+)\/(.+)$/.exec(path);
    if (fileMatch) {
      const data = fileMatch[1] === BOT_TOKEN && files.get(fileMatch[2]!);
      res.writeHead(data ? 200 : 404).end(data || "not found");
      return;
    }
    const match = /^\/bot([^/]+)\/([A-Za-z]+)$/.exec(path);
    if (!match || match[1] !== BOT_TOKEN) {
      reply(res, 401, {
        ok: false,
        error_code: 401,
        description: "Unauthorized",
      });
      return;
    }
    const method = match[2]!;
    const payload = await readJson(req);
    calls.push({ method, payload });

    const failures = failMethods.get(method) ?? 0;
    if (failures > 0) {
      failMethods.set(method, failures - 1);
      reply(res, 500, {
        ok: false,
        error_code: 500,
        description: "Internal Server Error",
      });
      return;
    }

    // Telegram rejects rich markdown that renders to no text (e.g. a bare
    // `##` heading marker) instead of sending an empty message.
    const richMarkdown = (payload.rich_message as { markdown?: string } | undefined)
      ?.markdown;
    if (
      richMarkdown !== undefined &&
      !markdownToPlainText(richMarkdown).trim()
    ) {
      reply(res, 400, {
        ok: false,
        error_code: 400,
        description: "Bad Request: RICH_MESSAGE_EMPTY",
      });
      return;
    }

    switch (method) {
      case "getMe":
        return ok(res, {
          first_name: "Centaur",
          id: BOT_ID,
          is_bot: true,
          username: BOT_USERNAME,
        });
      case "getWebhookInfo":
        return ok(res, { pending_update_count: 0, url: "" });
      case "deleteWebhook":
      case "sendChatAction":
        return ok(res, true);
      case "setMessageReaction":
        return ok(res, true);
      case "getUpdates": {
        const offset = Number(payload.offset ?? 0);
        getUpdatesOffsets.push(offset);
        const ready = updates.filter(
          (update) => Number(update.update_id) >= offset,
        );
        if (ready.length === 0) await new Promise((r) => setTimeout(r, 50));
        return ok(res, ready);
      }
      case "getFile":
        return ok(res, {
          file_id: payload.file_id,
          file_path: `files/${String(payload.file_id)}`,
          file_unique_id: `u-${String(payload.file_id)}`,
        });
      case "sendMessage":
      case "sendRichMessage":
        return ok(res, sendMessage(payload));
      case "editMessageText": {
        const key = `${Number(payload.chat_id)}:${Number(payload.message_id)}`;
        const existing = messages.get(key);
        if (!existing) {
          reply(res, 400, {
            ok: false,
            error_code: 400,
            description: "Bad Request: message to edit not found",
          });
          return;
        }
        const rich = payload.rich_message as { markdown?: string } | undefined;
        existing.text = String(rich?.markdown ?? payload.text ?? "");
        return ok(res, {
          chat: { id: existing.chatId, type: "private" },
          date: Math.floor(Date.now() / 1000),
          edit_date: Math.floor(Date.now() / 1000),
          from: { first_name: "Centaur", id: BOT_ID, is_bot: true },
          message_id: existing.messageId,
          text: existing.text,
        });
      }
      default:
        reply(res, 404, {
          ok: false,
          error_code: 404,
          description: `Not Found: method ${method} not found`,
        });
    }
  };

  const server: HttpServer = createServer((req, res) => {
    handle(req, res).catch((error) => {
      reply(res, 500, { ok: false, description: String(error) });
    });
  });
  await listen(server, port);

  return {
    calls,
    botMessages(chatId) {
      return Array.from(messages.values())
        .filter((message) => message.chatId === chatId)
        .sort((left, right) => left.messageId - right.messageId);
    },
    async close() {
      await closeServer(server);
    },
    enqueueUpdate(update) {
      updates.push(update);
    },
    failMethods,
    files,
    getUpdatesOffsets,
    reactions(chatId, messageId) {
      return calls
        .filter(
          (call) =>
            call.method === "setMessageReaction" &&
            Number(call.payload.chat_id) === chatId &&
            Number(call.payload.message_id) === messageId,
        )
        .map((call) => {
          const [reaction] = (call.payload.reaction ?? []) as {
            emoji?: string;
          }[];
          return reaction?.emoji ?? "";
        });
    },
    reset() {
      calls.length = 0;
      messages.clear();
      updates.length = 0;
      getUpdatesOffsets.length = 0;
      failMethods.clear();
      files.clear();
    },
    url,
  };
}

function ok(res: ServerResponse, result: unknown): void {
  reply(res, 200, { ok: true, result });
}

function reply(res: ServerResponse, status: number, body: unknown): void {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

async function readJson(
  req: IncomingMessage,
): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  const raw = Buffer.concat(chunks).toString("utf8");
  if (!raw) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object"
      ? (parsed as Record<string, unknown>)
      : {};
  } catch {
    return {};
  }
}
