import { createLinearbot, type LinearbotOptions } from "./index";

const port = numberEnv("PORT", 3001);
const apiUrl = stringEnv("CENTAUR_API_URL", "http://127.0.0.1:8080");
// Distinct from the api-rs `linear_webhook` workflow's LINEAR_WEBHOOK_SECRET:
// linearbot is a separate Linear webhook (different URL → different signing
// secret), so it gets its own key to avoid clobbering the workflow's.
const linearWebhookSecret = requiredEnv("LINEARBOT_WEBHOOK_SECRET");
// actor=app OAuth token (the bot runs as an app); a personal API key runs the
// same comment-thread model as a regular user, without an OAuth install.
const linearAccessToken = optionalEnv("LINEAR_ACCESS_TOKEN");
const linearApiKey = optionalEnv("LINEAR_API_KEY");
if (!linearAccessToken && !linearApiKey) {
  throw new Error("LINEAR_ACCESS_TOKEN (or LINEAR_API_KEY) is required");
}

// Default to info: the chat adapter logs raw webhook bodies at debug, and
// JSON-serializing those payloads on the hot path blocks the event loop.
const LOG_LEVELS = ["debug", "info", "warn", "error"] as const;
const minLogLevel: (typeof LOG_LEVELS)[number] = (() => {
  const value = optionalEnv("LINEARBOT_LOG_LEVEL")?.toLowerCase();
  return (LOG_LEVELS as readonly string[]).includes(value ?? "")
    ? (value as (typeof LOG_LEVELS)[number])
    : "info";
})();

const consoleLogger = {
  debug: (message: string, data?: unknown) => log("debug", message, data),
  info: (message: string, data?: unknown) => log("info", message, data),
  warn: (message: string, data?: unknown) => log("warn", message, data),
  error: (message: string, data?: unknown) => log("error", message, data),
  child: () => consoleLogger,
};

// slackbotv2 leaves the postgres URL optional, in which case pg.Pool silently
// falls back to localhost and every handler fails at runtime. Fail fast at
// boot instead — the chart always provides LINEARBOT_DATABASE_URL.
const postgresUrl =
  optionalEnv("LINEARBOT_DATABASE_URL") ??
  optionalEnv("DATABASE_URL") ??
  optionalEnv("POSTGRES_URL");
if (!postgresUrl) {
  throw new Error(
    "LINEARBOT_DATABASE_URL (or DATABASE_URL / POSTGRES_URL) is required",
  );
}

const options: LinearbotOptions = {
  apiUrl,
  apiKey: optionalEnv("LINEARBOT_API_KEY"),
  defaultHarnessType: optionalEnv("LINEARBOT_DEFAULT_HARNESS"),
  idleTimeoutMs: optionalNumberEnv("SESSION_IDLE_TIMEOUT_MS"),
  linearAccessToken,
  linearApiKey,
  linearApiUrl: optionalEnv("LINEAR_API_URL"),
  linearWebhookSecret,
  maxDurationMs: optionalNumberEnv("SESSION_MAX_DURATION_MS"),
  postgresUrl,
  stateKeyPrefix: optionalEnv("LINEARBOT_STATE_KEY_PREFIX"),
  userName: stringEnv("LINEARBOT_USER_NAME", "centaur"),
  logger: consoleLogger,
};

const { app, chat } = createLinearbot(options);
const server = Bun.serve({ port, fetch: app.fetch });

log("info", "linearbot_started", {
  port: server.port,
  api_url: apiUrl,
});

const shutdown = async (signal: string): Promise<void> => {
  log("info", "linearbot_shutdown_started", { signal });
  await chat.shutdown().catch(() => undefined);
  server.stop();
  log("info", "linearbot_shutdown_complete", { signal });
  process.exit(0);
};
process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));

function optionalEnv(name: string): string | undefined {
  const value = process.env[name]?.trim();
  return value ? value : undefined;
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

function log(
  level: (typeof LOG_LEVELS)[number],
  message: string,
  data?: unknown,
): void {
  if (LOG_LEVELS.indexOf(level) < LOG_LEVELS.indexOf(minLogLevel)) return;
  console.log(
    JSON.stringify({
      level,
      service: "linearbot",
      timestamp: new Date().toISOString(),
      event: message,
      ...(data && typeof data === "object"
        ? (data as Record<string, unknown>)
        : {}),
    }),
  );
}
