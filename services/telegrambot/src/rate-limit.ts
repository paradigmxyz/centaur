import { isTelegramRateLimitError } from "./telegram-api";
import type {
  EditMessageTextParams,
  SendMessageParams,
  TelegramApi,
} from "./telegram-api";
import type { Logger, TelegramMessage } from "./types";

/**
 * Coordinated outbound rate control for the Bot API.
 *
 * Telegram delta: Discord returns proactive bucket headers the SDK can obey,
 * but the Bot API publishes only soft guidance (~1 message/s per chat,
 * 20 messages/min per group) and punishes overruns with 429 + retry_after.
 * So mutations are scheduled locally — per-chat FIFO queues with sendMessage
 * pacing and a group message budget — and every method reactively honors its
 * own 429s. The 20/min quota applies to *messages*; edits, reactions, and
 * chat actions ride the same FIFO (ordering) but are never charged against it.
 *
 * Reads (getMe/getUpdates/deleteWebhook/getFile/downloadFile) pass through
 * untouched: they have no per-chat quota and must not sit behind a paced send.
 */

const MESSAGE_INTERVAL_MS = 1_000;
const GROUP_MESSAGES_PER_MINUTE = 20;
const GROUP_BUDGET_WINDOW_MS = 60_000;
const MAX_RATE_LIMIT_RETRIES = 5;
// Fallback when a 429 arrives without parameters.retry_after.
const DEFAULT_RETRY_AFTER_SECONDS = 1;

export type RateLimitClock = {
  now(): number;
  sleep(ms: number): Promise<void>;
};

export type RateLimitOptions = {
  /** Injectable clock/sleep so tests drive pacing deterministically. */
  clock?: RateLimitClock;
  /** sendMessage budget per group chat per minute (default 20). */
  groupMessagesPerMinute?: number;
  /** Additive backoff jitter in ms so concurrent chats don't retry in lockstep. */
  jitterMs?: () => number;
  /** 429 retries per call before the error is rethrown (default 5). */
  maxRetries?: number;
  /** Minimum gap between sendMessage calls in one chat (default 1000ms). */
  messageIntervalMs?: number;
};

export type TelegramMutationMethod =
  | "sendMessage"
  | "editMessageText"
  | "setMessageReaction"
  | "sendChatAction";

export type TelegramSendPriority = "normal" | "high";

export type TelegramSendQueueTask<T> = {
  chatId: number | string;
  method: TelegramMutationMethod;
  /**
   * Tasks sharing a coalesce key supersede each other: enqueueing marks every
   * not-yet-in-flight pending task with the same key as superseded (its
   * promise resolves with `undefined` immediately). Only use for calls whose
   * result type is `void` — in practice, editMessageText keyed by
   * chat+message_id, where the newest body is the only one worth sending.
   */
  coalesceKey?: string;
  /** "high" schedules ahead of queued normal work (terminal answer sends). */
  priority?: TelegramSendPriority;
  run: () => Promise<T>;
};

type QueueTask = {
  coalesceKey: string | undefined;
  inFlight: boolean;
  method: TelegramMutationMethod;
  priority: TelegramSendPriority;
  reject: (error: unknown) => void;
  resolve: (value: unknown) => void;
  run: () => Promise<unknown>;
  superseded: boolean;
};

type ChatState = {
  current: QueueTask | null;
  draining: boolean;
  isGroup: boolean;
  lastMessageAtMs: number;
  /** clock timestamps of sendMessage successes inside the sliding window. */
  messageWindow: number[];
  tasks: QueueTask[];
};

/**
 * Per-chat FIFO scheduler for outbound Bot API mutations. One worker per chat
 * drains its queue serially — a rate-limited chat blocks only itself — while
 * priority insertion lets terminal sends jump queued progress edits and
 * coalesce keys drop stale edit bodies before they ever hit the network.
 *
 * Exported separately from the wrapper so the answer streamer can enqueue its
 * own work (custom priorities/coalescing) on the same budget.
 */
export class TelegramSendQueue {
  private readonly logger: Logger;
  private readonly clock: RateLimitClock;
  private readonly jitterMs: () => number;
  private readonly maxRetries: number;
  private readonly messageIntervalMs: number;
  private readonly groupMessagesPerMinute: number;
  private readonly chats = new Map<string, ChatState>();

  constructor(logger: Logger, options: RateLimitOptions = {}) {
    this.logger = logger;
    this.clock = options.clock ?? {
      now: () => Date.now(),
      sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    };
    this.jitterMs =
      options.jitterMs ?? (() => Math.floor(Math.random() * 250));
    this.maxRetries = options.maxRetries ?? MAX_RATE_LIMIT_RETRIES;
    this.messageIntervalMs = options.messageIntervalMs ?? MESSAGE_INTERVAL_MS;
    this.groupMessagesPerMinute =
      options.groupMessagesPerMinute ?? GROUP_MESSAGES_PER_MINUTE;
  }

  enqueue<T>(input: TelegramSendQueueTask<T>): Promise<T> {
    const chat = this.chatState(input.chatId);
    return new Promise<T>((resolve, reject) => {
      const task: QueueTask = {
        coalesceKey: input.coalesceKey,
        inFlight: false,
        method: input.method,
        priority: input.priority ?? "normal",
        reject,
        resolve: resolve as (value: unknown) => void,
        run: input.run,
        superseded: false,
      };
      if (task.coalesceKey) {
        this.supersede(chat, input.chatId, task.coalesceKey, "replaced");
      }
      this.insert(chat, task);
      if (!chat.draining) void this.drain(chat, input.chatId);
    });
  }

  /**
   * Supersede pending tasks for a coalesce key without enqueueing a
   * replacement — e.g. progress edits made moot by a delivered terminal
   * answer. Returns how many pending tasks were dropped.
   */
  cancel(chatId: number | string, coalesceKey: string): number {
    const chat = this.chats.get(chatKey(chatId));
    if (!chat) return 0;
    return this.supersede(chat, chatId, coalesceKey, "cancelled");
  }

  private chatState(chatId: number | string): ChatState {
    const key = chatKey(chatId);
    let chat = this.chats.get(key);
    if (!chat) {
      chat = {
        current: null,
        draining: false,
        isGroup: isGroupChatId(chatId),
        lastMessageAtMs: Number.NEGATIVE_INFINITY,
        messageWindow: [],
        tasks: [],
      };
      this.chats.set(key, chat);
    }
    return chat;
  }

  private insert(chat: ChatState, task: QueueTask): void {
    if (task.priority === "high") {
      // Ahead of queued normal work, behind earlier high-priority work
      // (FIFO within a priority level); never preempts the in-flight task.
      const index = chat.tasks.findIndex((t) => t.priority !== "high");
      if (index === -1) chat.tasks.push(task);
      else chat.tasks.splice(index, 0, task);
      return;
    }
    chat.tasks.push(task);
  }

  private supersede(
    chat: ChatState,
    chatId: number | string,
    coalesceKey: string,
    reason: "replaced" | "cancelled",
  ): number {
    // The current task is cancellable while it is between attempts (waiting
    // out pacing or a 429 backoff); once a request is actually in flight the
    // send may already have happened, so it must settle on its own.
    const candidates =
      chat.current && !chat.current.inFlight
        ? [chat.current, ...chat.tasks]
        : chat.tasks;
    let dropped = 0;
    for (const task of candidates) {
      if (task.superseded || task.coalesceKey !== coalesceKey) continue;
      task.superseded = true;
      task.resolve(undefined);
      dropped += 1;
    }
    if (dropped) {
      this.logger.debug("telegrambot_edit_superseded", {
        chat_id: chatKey(chatId),
        coalesce_key: coalesceKey,
        dropped,
        reason,
      });
    }
    return dropped;
  }

  private async drain(
    chat: ChatState,
    chatId: number | string,
  ): Promise<void> {
    chat.draining = true;
    try {
      let task: QueueTask | undefined;
      while ((task = chat.tasks.shift())) {
        if (task.superseded) continue;
        chat.current = task;
        await this.runTask(chat, chatId, task);
        chat.current = null;
      }
    } finally {
      chat.draining = false;
      chat.current = null;
    }
  }

  /** Never throws — the task settles via its own resolve/reject. */
  private async runTask(
    chat: ChatState,
    chatId: number | string,
    task: QueueTask,
  ): Promise<void> {
    let retries = 0;
    for (;;) {
      // A task superseded while it waited (its promise already resolved)
      // must never reach the network with its stale body.
      if (task.superseded) return;
      if (task.method === "sendMessage") await this.awaitSendBudget(chat, chatId);
      if (task.superseded) return;
      task.inFlight = true;
      try {
        const result = await task.run();
        if (task.method === "sendMessage") this.recordSend(chat);
        task.resolve(result);
        return;
      } catch (error) {
        if (!isTelegramRateLimitError(error)) {
          task.reject(error);
          return;
        }
        const retryAfterSeconds =
          error.retryAfterSeconds ?? DEFAULT_RETRY_AFTER_SECONDS;
        // Method-specific telemetry; never the URL (it embeds the token).
        this.logger.warn("telegrambot_rate_limited", {
          attempt: retries + 1,
          chat_id: chatKey(chatId),
          method: task.method,
          retry_after: retryAfterSeconds,
        });
        if (retries >= this.maxRetries) {
          task.reject(error);
          return;
        }
        retries += 1;
        task.inFlight = false;
        await this.clock.sleep(
          retryAfterSeconds * 1_000 + Math.max(0, this.jitterMs()),
        );
      }
    }
  }

  /** Blocks until this chat may send a new message (pacing + group budget). */
  private async awaitSendBudget(
    chat: ChatState,
    chatId: number | string,
  ): Promise<void> {
    for (;;) {
      const now = this.clock.now();
      const paceWaitMs =
        chat.lastMessageAtMs + this.messageIntervalMs - now;
      let budgetWaitMs = 0;
      if (chat.isGroup) {
        this.pruneWindow(chat, now);
        if (chat.messageWindow.length >= this.groupMessagesPerMinute) {
          const oldest = chat.messageWindow[0];
          if (oldest !== undefined) {
            budgetWaitMs = oldest + GROUP_BUDGET_WINDOW_MS - now;
          }
        }
      }
      const waitMs = Math.max(paceWaitMs, budgetWaitMs);
      if (waitMs <= 0) return;
      this.logger.debug("telegrambot_send_paced", {
        chat_id: chatKey(chatId),
        reason:
          budgetWaitMs > paceWaitMs ? "group_budget" : "message_interval",
        wait_ms: waitMs,
      });
      await this.clock.sleep(waitMs);
    }
  }

  private recordSend(chat: ChatState): void {
    const now = this.clock.now();
    chat.lastMessageAtMs = now;
    if (!chat.isGroup) return;
    chat.messageWindow.push(now);
    this.pruneWindow(chat, now);
  }

  private pruneWindow(chat: ChatState, now: number): void {
    const cutoff = now - GROUP_BUDGET_WINDOW_MS;
    while (chat.messageWindow.length) {
      const oldest = chat.messageWindow[0];
      if (oldest === undefined || oldest > cutoff) break;
      chat.messageWindow.shift();
    }
  }
}

export type RateLimitedTelegramApi = TelegramApi & {
  /** Drop queued progress edits for a message the streamer no longer needs. */
  cancelPendingEdits(chatId: number | string, messageId: number): number;
  /** Terminal in-place answer edit: jumps queued progress edits and coalesces them away. */
  editMessageTextUrgent(params: EditMessageTextParams): Promise<void>;
  /** Shared scheduler, exposed so the streamer can enqueue custom work on the same budget. */
  queue: TelegramSendQueue;
  /** Terminal answer send: schedules ahead of queued progress edits. */
  sendMessageUrgent(params: SendMessageParams): Promise<TelegramMessage>;
};

export function createRateLimitedTelegramApi(
  api: TelegramApi,
  logger: Logger,
  options: RateLimitOptions = {},
): RateLimitedTelegramApi {
  const queue = new TelegramSendQueue(logger, options);

  const sendMessage = (
    params: SendMessageParams,
    priority: TelegramSendPriority,
  ): Promise<TelegramMessage> =>
    queue.enqueue({
      chatId: params.chat_id,
      method: "sendMessage",
      priority,
      run: () => api.sendMessage(params),
    });

  const editMessageText = (
    params: EditMessageTextParams,
    priority: TelegramSendPriority,
  ): Promise<void> =>
    queue.enqueue({
      chatId: params.chat_id,
      coalesceKey: editCoalesceKey(params.chat_id, params.message_id),
      method: "editMessageText",
      priority,
      run: () => api.editMessageText(params),
    });

  return {
    // Reads pass through: no per-chat quota, must not queue behind sends.
    getMe: () => api.getMe(),
    getUpdates: (params, signal) => api.getUpdates(params, signal),
    deleteWebhook: (params) => api.deleteWebhook(params),
    getFile: (fileId) => api.getFile(fileId),
    downloadFile: (filePath, signal) => api.downloadFile(filePath, signal),

    sendMessage: (params) => sendMessage(params, "normal"),
    sendMessageUrgent: (params) => sendMessage(params, "high"),
    editMessageText: (params) => editMessageText(params, "normal"),
    editMessageTextUrgent: (params) => editMessageText(params, "high"),
    setMessageReaction: (params) =>
      queue.enqueue({
        chatId: params.chat_id,
        method: "setMessageReaction",
        run: () => api.setMessageReaction(params),
      }),
    sendChatAction: (params) =>
      queue.enqueue({
        chatId: params.chat_id,
        method: "sendChatAction",
        run: () => api.sendChatAction(params),
      }),

    cancelPendingEdits: (chatId, messageId) =>
      queue.cancel(chatId, editCoalesceKey(chatId, messageId)),
    queue,
  };
}

function editCoalesceKey(
  chatId: number | string,
  messageId: number,
): string {
  return `edit:${chatKey(chatId)}:${messageId}`;
}

function chatKey(chatId: number | string): string {
  return String(chatId);
}

/**
 * Telegram encodes chat kind in the id's sign: groups/supergroups/channels are
 * negative, private chats positive. `@channelusername` strings are treated as
 * group-like conservatively (the 20/min budget applies to them too).
 */
function isGroupChatId(chatId: number | string): boolean {
  if (typeof chatId === "number") return chatId < 0;
  if (chatId.startsWith("@")) return true;
  const numeric = Number(chatId);
  return Number.isFinite(numeric) && numeric < 0;
}
