import { createTelegrambot } from "./index";
import type { TelegrambotOptions } from "./types";

const port = numberEnv("PORT", 3001);
const apiUrl = stringEnv("CENTAUR_API_URL", "http://127.0.0.1:8080");
// The bot token IS the platform credential (embedded in every Bot API URL
// path); it must never be logged, and telegram-api.ts redacts it defensively.
const botToken = requiredEnv("TELEGRAM_BOT_TOKEN");

const consoleLogger = {
  debug: (message: string, data?: unknown) => log("debug", message, data),
  info: (message: string, data?: unknown) => log("info", message, data),
  warn: (message: string, data?: unknown) => log("warn", message, data),
  error: (message: string, data?: unknown) => log("error", message, data),
  child: () => consoleLogger,
};

// Telegram delta mirrors the Discord one: pg.Pool silently falls back to
// localhost when no URL is provided and every handler fails at runtime. Fail
// fast at boot instead — the chart always provides TELEGRAMBOT_DATABASE_URL.
const postgresUrl =
  optionalEnv("TELEGRAMBOT_DATABASE_URL") ??
  optionalEnv("DATABASE_URL") ??
  optionalEnv("POSTGRES_URL");
if (!postgresUrl) {
  throw new Error(
    "TELEGRAMBOT_DATABASE_URL (or DATABASE_URL / POSTGRES_URL) is required",
  );
}

const options: TelegrambotOptions = {
  answerEditIntervalMs: optionalNumberEnv("TELEGRAMBOT_ANSWER_EDIT_INTERVAL_MS"),
  apiKey: optionalEnv("TELEGRAMBOT_API_KEY"),
  apiUrl,
  botToken,
  chatAllowlist: optionalList("TELEGRAMBOT_CHAT_ALLOWLIST"),
  idleTimeoutMs: optionalNumberEnv("SESSION_IDLE_TIMEOUT_MS"),
  leaseTtlMs: optionalNumberEnv("TELEGRAMBOT_LEASE_TTL_MS"),
  logger: consoleLogger,
  maxConcurrentThreads: optionalNumberEnv("TELEGRAMBOT_MAX_CONCURRENT_THREADS"),
  maxDurationMs: optionalNumberEnv("SESSION_MAX_DURATION_MS"),
  pollTimeoutSeconds: optionalNumberEnv("TELEGRAMBOT_POLL_TIMEOUT_S"),
  postgresUrl,
  retentionHours: optionalNumberEnv("TELEGRAMBOT_RETENTION_HOURS"),
  telegramApiUrl: optionalEnv("TELEGRAM_API_URL"),
  userAllowlist: optionalList("TELEGRAMBOT_USER_ALLOWLIST"),
  userName: stringEnv("TELEGRAMBOT_USER_NAME", "centaur"),
};

const bot = createTelegrambot(options);
const server = Bun.serve({ port, fetch: bot.app.fetch });

log("info", "telegrambot_started", {
  port: server.port,
  api_url: apiUrl,
});

const shutdown = async (signal: string): Promise<void> => {
  log("info", "telegrambot_shutdown_started", { signal });
  await bot.shutdown().catch(() => undefined);
  server.stop();
  log("info", "telegrambot_shutdown_complete", { signal });
  process.exit(0);
};
process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));

await bot.start();

function optionalEnv(name: string): string | undefined {
  const value = process.env[name]?.trim();
  return value ? value : undefined;
}

function optionalList(name: string): string[] | undefined {
  const value = optionalEnv(name);
  if (!value) return undefined;
  return value
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean);
}

function requiredEnv(name: string): string {
  const value = optionalEnv(name);
  if (!value) {
    throw new Error(`${name} is required`);
  }
  return value;
}

function stringEnv(name: string, fallback: string): string {
  return optionalEnv(name) ?? fallback;
}

function numberEnv(name: string, fallback: number): number {
  return optionalNumberEnv(name) ?? fallback;
}

function optionalNumberEnv(name: string): number | undefined {
  const value = optionalEnv(name);
  if (!value) return undefined;
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

function log(level: string, message: string, data?: unknown): void {
  console.log(
    JSON.stringify({
      level,
      service: "telegrambot",
      timestamp: new Date().toISOString(),
      event: message,
      ...(data && typeof data === "object"
        ? (data as Record<string, unknown>)
        : {}),
    }),
  );
}
