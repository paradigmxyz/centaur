import type {
  JsonObject,
  JsonValue,
  TelegramFile,
  TelegramMessage,
  TelegramUpdate,
  TelegramUser,
  TelegrambotFetch,
  TelegrambotOptions,
} from "./types";
import { errorMessage } from "./utils";

export const DEFAULT_TELEGRAM_API_URL = "https://api.telegram.org";

/**
 * Telegram delta: the Bot API embeds the token in the URL path
 * (`/bot<token>/<method>`), so no error message, log line, or wrapped cause in
 * this module may ever carry a request URL. Errors are rebuilt from the method
 * name plus the API's own `description`, and any accidental token occurrence
 * in an underlying cause is redacted defensively.
 */
export class TelegramApiError extends Error {
  readonly method: string;
  /** HTTP status (0 for transport-level failures). */
  readonly status: number;
  /** Telegram `error_code` when the API returned a structured error. */
  readonly errorCode: number | undefined;
  readonly description: string | undefined;
  /** `parameters.retry_after` from a 429, in seconds. */
  readonly retryAfterSeconds: number | undefined;

  constructor(input: {
    method: string;
    status: number;
    errorCode?: number;
    description?: string;
    retryAfterSeconds?: number;
  }) {
    super(
      `telegram ${input.method} failed: ${input.status}${
        input.description ? ` ${input.description}` : ""
      }`,
    );
    this.name = "TelegramApiError";
    this.method = input.method;
    this.status = input.status;
    this.errorCode = input.errorCode;
    this.description = input.description;
    this.retryAfterSeconds = input.retryAfterSeconds;
  }
}

/** 401/404 mean a bad token; 409 means a competing poller/webhook. Both are
 * configuration faults the process should surface by exiting, not retrying. */
export function isFatalTelegramAuthError(error: unknown): boolean {
  return (
    error instanceof TelegramApiError &&
    (error.status === 401 || error.status === 404 || error.status === 409)
  );
}

export function isTelegramRateLimitError(
  error: unknown,
): error is TelegramApiError {
  return error instanceof TelegramApiError && error.status === 429;
}

/**
 * Parse/entity rejections (400 "can't parse entities…") — the render fallback
 * regenerates plain text instead of resending the same malformed body.
 */
export function isTelegramParseError(error: unknown): boolean {
  return (
    error instanceof TelegramApiError &&
    error.status === 400 &&
    /parse|entit/i.test(error.description ?? "")
  );
}

export type SendMessageParams = {
  chat_id: number | string;
  text: string;
  parse_mode?: "HTML";
  message_thread_id?: number;
  reply_parameters?: {
    message_id: number;
    /** Send anyway when the replied-to message no longer exists — Telegram
     * otherwise rejects the whole send, wedging retried deliveries. */
    allow_sending_without_reply?: boolean;
  };
  link_preview_options?: { is_disabled?: boolean };
};

export type EditMessageTextParams = {
  chat_id: number | string;
  message_id: number;
  text: string;
  parse_mode?: "HTML";
  link_preview_options?: { is_disabled?: boolean };
};

export type SetMessageReactionParams = {
  chat_id: number | string;
  message_id: number;
  reaction: Array<{ type: "emoji"; emoji: string }>;
};

export type SendChatActionParams = {
  chat_id: number | string;
  action: "typing";
  message_thread_id?: number;
};

export type GetUpdatesParams = {
  offset?: number;
  timeout?: number;
  allowed_updates?: string[];
};

export type TelegramApi = {
  getMe(): Promise<TelegramUser>;
  getUpdates(
    params: GetUpdatesParams,
    signal?: AbortSignal,
  ): Promise<TelegramUpdate[]>;
  deleteWebhook(params: { drop_pending_updates: boolean }): Promise<void>;
  sendMessage(params: SendMessageParams): Promise<TelegramMessage>;
  editMessageText(params: EditMessageTextParams): Promise<void>;
  setMessageReaction(params: SetMessageReactionParams): Promise<void>;
  sendChatAction(params: SendChatActionParams): Promise<void>;
  getFile(fileId: string): Promise<TelegramFile>;
  /** Download a file previously located via getFile. */
  downloadFile(filePath: string, signal?: AbortSignal): Promise<Uint8Array>;
};

export function createTelegramApi(
  options: Pick<
    TelegrambotOptions,
    "botToken" | "telegramApiUrl" | "fetch" | "pollTimeoutSeconds"
  >,
): TelegramApi {
  const fetchFn: TelegrambotFetch = options.fetch ?? fetch;
  const apiBase = (options.telegramApiUrl ?? DEFAULT_TELEGRAM_API_URL).replace(
    /\/$/,
    "",
  );
  const token = options.botToken;

  const call = async <T>(
    method: string,
    params?: JsonObject,
    init?: { signal?: AbortSignal; timeoutMs?: number },
  ): Promise<T> => {
    let response: Response;
    try {
      response = await fetchFn(`${apiBase}/bot${token}/${method}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: params ? JSON.stringify(params) : undefined,
        signal: init?.signal ?? timeoutSignal(init?.timeoutMs),
      });
    } catch (error) {
      // Transport failure: rebuild the message so a URL-bearing cause (which
      // would include the token) never propagates.
      throw new TelegramApiError({
        method,
        status: 0,
        description: redactToken(errorMessage(error), token),
      });
    }

    let body: {
      ok?: boolean;
      result?: JsonValue;
      description?: string;
      error_code?: number;
      parameters?: { retry_after?: number };
    };
    try {
      body = (await response.json()) as typeof body;
    } catch {
      throw new TelegramApiError({ method, status: response.status });
    }

    if (!response.ok || body.ok !== true) {
      throw new TelegramApiError({
        method,
        status: response.status,
        errorCode: body.error_code,
        description: body.description,
        retryAfterSeconds: body.parameters?.retry_after,
      });
    }
    return body.result as T;
  };

  const pollTimeoutSeconds = options.pollTimeoutSeconds ?? 50;

  return {
    getMe: () => call<TelegramUser>("getMe", undefined, { timeoutMs: 15_000 }),

    getUpdates: (params, signal) =>
      call<TelegramUpdate[]>(
        "getUpdates",
        { timeout: pollTimeoutSeconds, ...params } as JsonObject,
        {
          signal,
          // Grace beyond the server-side long-poll window so a healthy but
          // slow response is not aborted at exactly the poll timeout.
          timeoutMs: ((params.timeout ?? pollTimeoutSeconds) + 15) * 1000,
        },
      ),

    deleteWebhook: async (params) => {
      await call<boolean>("deleteWebhook", params as JsonObject, {
        timeoutMs: 15_000,
      });
    },

    sendMessage: (params) =>
      call<TelegramMessage>("sendMessage", params as JsonObject, {
        timeoutMs: 30_000,
      }),

    editMessageText: async (params) => {
      await call<JsonValue>("editMessageText", params as JsonObject, {
        timeoutMs: 30_000,
      });
    },

    setMessageReaction: async (params) => {
      await call<boolean>("setMessageReaction", params as JsonObject, {
        timeoutMs: 15_000,
      });
    },

    sendChatAction: async (params) => {
      await call<boolean>("sendChatAction", params as JsonObject, {
        timeoutMs: 15_000,
      });
    },

    getFile: (fileId) =>
      call<TelegramFile>(
        "getFile",
        { file_id: fileId },
        { timeoutMs: 30_000 },
      ),

    downloadFile: async (filePath, signal) => {
      let response: Response;
      try {
        response = await fetchFn(`${apiBase}/file/bot${token}/${filePath}`, {
          signal: signal ?? timeoutSignal(60_000),
        });
      } catch (error) {
        throw new TelegramApiError({
          method: "downloadFile",
          status: 0,
          description: redactToken(errorMessage(error), token),
        });
      }
      if (!response.ok) {
        throw new TelegramApiError({
          method: "downloadFile",
          status: response.status,
        });
      }
      return new Uint8Array(await response.arrayBuffer());
    },
  };
}

function timeoutSignal(timeoutMs?: number): AbortSignal | undefined {
  return timeoutMs ? AbortSignal.timeout(timeoutMs) : undefined;
}

function redactToken(text: string, token: string): string {
  return token ? text.split(token).join("<redacted>") : text;
}
