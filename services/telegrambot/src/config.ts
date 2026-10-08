import type { TelegrambotMode, TelegrambotOptions } from "./types";
import { splitEnvList } from "./utils";

export type TelegrambotConfig = {
  options: TelegrambotOptions;
  port: number;
};

type Env = Record<string, string | undefined>;

/**
 * Reads and validates the runtime environment. Throws on anything that would
 * otherwise fail late or fail open: a missing token or database, an unknown
 * mode, or webhook mode without a secret token.
 */
export function loadTelegrambotConfig(env: Env): TelegrambotConfig {
  const read = (name: string): string | undefined => {
    const value = env[name]?.trim();
    return value ? value : undefined;
  };
  const required = (name: string): string => {
    const value = read(name);
    if (!value) throw new Error(`${name} is required`);
    return value;
  };
  const positiveInt = (name: string): number | undefined => {
    const value = read(name);
    if (!value) return undefined;
    const parsed = Number.parseInt(value, 10);
    if (!Number.isFinite(parsed) || parsed <= 0 || String(parsed) !== value) {
      throw new Error(`${name} must be a positive integer`);
    }
    return parsed;
  };
  const list = (name: string): string[] => {
    const values = splitEnvList(read(name));
    for (const value of values) {
      if (!/^-?\d+$/.test(value)) {
        throw new Error(`${name} must contain numeric Telegram ids`);
      }
    }
    return values;
  };

  const mode = (read("TELEGRAMBOT_MODE") ?? "polling") as TelegrambotMode;
  if (mode !== "polling" && mode !== "webhook") {
    throw new Error('TELEGRAMBOT_MODE must be "polling" or "webhook"');
  }
  const webhookSecretToken = read("TELEGRAM_WEBHOOK_SECRET_TOKEN");
  if (mode === "webhook" && !webhookSecretToken) {
    throw new Error(
      "TELEGRAM_WEBHOOK_SECRET_TOKEN is required when TELEGRAMBOT_MODE=webhook",
    );
  }

  // No silent localhost fallback: pg.Pool would otherwise fail every handler
  // at runtime instead of at boot.
  const postgresUrl =
    read("TELEGRAMBOT_DATABASE_URL") ??
    read("DATABASE_URL") ??
    read("POSTGRES_URL");
  if (!postgresUrl) {
    throw new Error(
      "TELEGRAMBOT_DATABASE_URL (or DATABASE_URL / POSTGRES_URL) is required",
    );
  }

  return {
    port: positiveInt("PORT") ?? 3001,
    options: {
      activeExecutionTtlMs: positiveInt("TELEGRAMBOT_ACTIVE_EXECUTION_TTL_MS"),
      answerEditIntervalMs: positiveInt("TELEGRAMBOT_ANSWER_EDIT_INTERVAL_MS"),
      apiKey: read("TELEGRAMBOT_API_KEY"),
      apiUrl: read("CENTAUR_API_URL") ?? "http://127.0.0.1:8080",
      botToken: required("TELEGRAM_BOT_TOKEN"),
      chatAllowlist: list("TELEGRAMBOT_CHAT_ALLOWLIST"),
      idleTimeoutMs: positiveInt("SESSION_IDLE_TIMEOUT_MS"),
      maxDurationMs: positiveInt("SESSION_MAX_DURATION_MS"),
      mode,
      pollTimeoutSeconds: positiveInt("TELEGRAMBOT_POLL_TIMEOUT_SECONDS"),
      postgresUrl,
      stateKeyPrefix: read("TELEGRAMBOT_STATE_KEY_PREFIX"),
      telegramApiUrl: read("TELEGRAM_API_BASE_URL"),
      userAllowlist: list("TELEGRAMBOT_USER_ALLOWLIST"),
      userName: read("TELEGRAM_BOT_USERNAME"),
      webhookSecretToken,
    },
  };
}
