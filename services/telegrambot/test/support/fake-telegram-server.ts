import type { TelegramUpdate, TelegramUser } from "../../src/types";

/**
 * Bun.serve-based fake Telegram Bot API for unit and e2e tests. Routes
 * `POST /bot<token>/<method>`, records every call, and honors getUpdates
 * offset semantics the way the real API does: updates with `update_id` below
 * the supplied offset are confirmed (dropped forever), updates at or above it
 * are redelivered until a later poll confirms them. Failures (401/409/429/
 * 5xx) and hangs are scriptable per method so tests can drive the poller's
 * error taxonomy without a network.
 */

export type RecordedTelegramCall = {
  method: string;
  params: Record<string, unknown>;
  atMs: number;
};

export type ScriptedTelegramFailure = {
  /** HTTP status, echoed as Telegram's error_code. */
  status: number;
  description?: string;
  /** Included as parameters.retry_after (seconds), Telegram 429 style. */
  retryAfterSeconds?: number;
};

type Script = { kind: "http"; failure: ScriptedTelegramFailure } | { kind: "hang" };

type ScriptEntry = { script: Script; remaining: number };

export type FakeTelegramServer = {
  baseUrl: string;
  token: string;
  botUser: TelegramUser;
  /** All calls in arrival order, including scripted failures and hangs. */
  calls: RecordedTelegramCall[];
  callsFor(method: string): RecordedTelegramCall[];
  /** Queue updates for getUpdates delivery; wakes any waiting long poll. */
  queueUpdates(...updates: TelegramUpdate[]): void;
  /** update_ids confirmed (dropped) by a later-offset getUpdates call. */
  confirmedUpdateIds: number[];
  /** update_ids still queued (unconfirmed). */
  pendingUpdateIds(): number[];
  /** Script the next `times` calls to `method` to fail with a Telegram-shaped
   * error body. Use Number.POSITIVE_INFINITY + clearScripts for outages. */
  failNext(
    method: string,
    failure: ScriptedTelegramFailure,
    times?: number,
  ): void;
  /** Script the next `times` calls to `method` to hang until client abort or
   * server stop. */
  hangNext(method: string, times?: number): void;
  clearScripts(method?: string): void;
  stop(): void;
};

export function startFakeTelegramServer(
  options: { token?: string; botUser?: Partial<TelegramUser> } = {},
): FakeTelegramServer {
  const token = options.token ?? "fake-telegram-token";
  const botUser: TelegramUser = {
    id: 999_001,
    is_bot: true,
    first_name: "Fake Bot",
    username: "fake_bot",
    ...options.botUser,
  };

  const calls: RecordedTelegramCall[] = [];
  const scripts = new Map<string, ScriptEntry[]>();
  const confirmedUpdateIds: number[] = [];
  let pending: TelegramUpdate[] = [];
  let stopped = false;
  let nextMessageId = 1_000;

  // Long polls wake on queueUpdates or stop; hangs release only on stop (or
  // client abort) so queuing updates cannot un-hang a scripted hang.
  const pollWakers = new Set<() => void>();
  const hangReleases = new Set<() => void>();

  const wakePolls = () => {
    for (const wake of [...pollWakers]) wake();
  };

  const waitForWake = (ms: number, signal: AbortSignal | undefined) =>
    new Promise<void>((resolve) => {
      const finish = () => {
        pollWakers.delete(finish);
        clearTimeout(timer);
        signal?.removeEventListener("abort", finish);
        resolve();
      };
      const timer = setTimeout(finish, ms);
      pollWakers.add(finish);
      signal?.addEventListener("abort", finish, { once: true });
    });

  const ok = (result: unknown) => Response.json({ ok: true, result });
  const err = (failure: ScriptedTelegramFailure) =>
    Response.json(
      {
        ok: false,
        error_code: failure.status,
        description:
          failure.description ?? `fake telegram error ${failure.status}`,
        ...(failure.retryAfterSeconds !== undefined
          ? { parameters: { retry_after: failure.retryAfterSeconds } }
          : {}),
      },
      { status: failure.status },
    );

  const takeScript = (method: string): Script | undefined => {
    const queue = scripts.get(method);
    const entry = queue?.[0];
    if (!queue || !entry) return undefined;
    entry.remaining -= 1;
    if (entry.remaining <= 0) queue.shift();
    if (queue.length === 0) scripts.delete(method);
    return entry.script;
  };

  async function handleGetUpdates(
    params: Record<string, unknown>,
    signal: AbortSignal | undefined,
  ): Promise<TelegramUpdate[]> {
    const offsetRaw = params["offset"];
    const offset = typeof offsetRaw === "number" ? offsetRaw : undefined;
    if (offset !== undefined) {
      const dropped = pending.filter((update) => update.update_id < offset);
      if (dropped.length > 0) {
        confirmedUpdateIds.push(...dropped.map((update) => update.update_id));
        pending = pending.filter((update) => update.update_id >= offset);
      }
    }
    const timeoutRaw = params["timeout"];
    const timeoutMs = (typeof timeoutRaw === "number" ? timeoutRaw : 0) * 1000;
    const deadline = Date.now() + timeoutMs;
    while (!stopped && !signal?.aborted) {
      const available = pending.filter(
        (update) => offset === undefined || update.update_id >= offset,
      );
      if (available.length > 0) return available.slice(0, 100);
      const remainingMs = deadline - Date.now();
      if (remainingMs <= 0) return [];
      await waitForWake(Math.min(remainingMs, 5_000), signal);
    }
    return [];
  }

  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    // Above the longest realistic long-poll (Telegram caps timeout at 50s);
    // Bun's 10s default would sever a healthy hanging poll mid-test.
    idleTimeout: 255,
    async fetch(req) {
      const url = new URL(req.url);
      const match = /^\/bot([^/]+)\/([^/]+)$/.exec(url.pathname);
      if (!match || req.method !== "POST") {
        return new Response("not found", { status: 404 });
      }
      const requestToken = match[1] ?? "";
      const method = match[2] ?? "";

      let params: Record<string, unknown> = {};
      const bodyText = await req.text();
      if (bodyText) {
        try {
          params = JSON.parse(bodyText) as Record<string, unknown>;
        } catch {
          // keep {}: a malformed body is still a recorded call
        }
      }
      calls.push({ method, params, atMs: Date.now() });

      if (requestToken !== token) {
        return err({ status: 401, description: "Unauthorized" });
      }

      const script = takeScript(method);
      if (script) {
        if (script.kind === "hang") {
          await new Promise<void>((resolve) => {
            const finish = () => {
              hangReleases.delete(finish);
              req.signal?.removeEventListener("abort", finish);
              resolve();
            };
            hangReleases.add(finish);
            req.signal?.addEventListener("abort", finish, { once: true });
          });
          return err({ status: 503, description: "hang released" });
        }
        return err(script.failure);
      }

      switch (method) {
        case "getMe":
          return ok(botUser);
        case "getUpdates":
          return ok(await handleGetUpdates(params, req.signal));
        case "deleteWebhook":
          return ok(true);
        case "sendMessage": {
          const chatIdRaw = params["chat_id"];
          const chatId =
            typeof chatIdRaw === "number" ? chatIdRaw : Number(chatIdRaw ?? 0);
          return ok({
            message_id: nextMessageId++,
            date: Math.floor(Date.now() / 1000),
            chat: { id: chatId, type: "private" },
            text: typeof params["text"] === "string" ? params["text"] : "",
          });
        }
        case "editMessageText":
        case "setMessageReaction":
        case "sendChatAction":
          return ok(true);
        default:
          return err({
            status: 404,
            description: `Not Found: method ${method}`,
          });
      }
    },
  });

  return {
    baseUrl: `http://127.0.0.1:${server.port}`,
    token,
    botUser,
    calls,
    callsFor: (method) => calls.filter((call) => call.method === method),
    queueUpdates(...updates) {
      pending.push(...updates);
      pending.sort((a, b) => a.update_id - b.update_id);
      wakePolls();
    },
    confirmedUpdateIds,
    pendingUpdateIds: () => pending.map((update) => update.update_id),
    failNext(method, failure, times = 1) {
      const queue = scripts.get(method) ?? [];
      queue.push({ script: { kind: "http", failure }, remaining: times });
      scripts.set(method, queue);
    },
    hangNext(method, times = 1) {
      const queue = scripts.get(method) ?? [];
      queue.push({ script: { kind: "hang" }, remaining: times });
      scripts.set(method, queue);
    },
    clearScripts(method) {
      if (method === undefined) scripts.clear();
      else scripts.delete(method);
    },
    stop() {
      if (stopped) return;
      stopped = true;
      wakePolls();
      for (const release of [...hangReleases]) release();
      server.stop(true);
    },
  };
}
