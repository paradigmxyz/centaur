import { createGatewayController } from "./gateway";
import { createDiscordbot, type DiscordbotOptions } from "./index";

const port = numberEnv("PORT", 3001);
const apiUrl = stringEnv("CENTAUR_API_URL", "http://127.0.0.1:8080");
const botToken = requiredEnv("DISCORD_BOT_TOKEN");
const publicKey = requiredEnv("DISCORD_PUBLIC_KEY");
const applicationId = requiredEnv("DISCORD_APPLICATION_ID");

const consoleLogger = {
  debug: (message: string, data?: unknown) => log("debug", message, data),
  info: (message: string, data?: unknown) => log("info", message, data),
  warn: (message: string, data?: unknown) => log("warn", message, data),
  error: (message: string, data?: unknown) => log("error", message, data),
  child: () => consoleLogger,
};

const gateway = createGatewayController({ logger: consoleLogger });

// Discord delta: slackbotv2 leaves the postgres URL optional, in which case
// pg.Pool silently falls back to localhost and every handler fails at runtime.
// Fail fast at boot instead — the chart always provides DISCORDBOT_DATABASE_URL.
const postgresUrl =
  optionalEnv("DISCORDBOT_DATABASE_URL") ??
  optionalEnv("DATABASE_URL") ??
  optionalEnv("POSTGRES_URL");
if (!postgresUrl) {
  throw new Error(
    "DISCORDBOT_DATABASE_URL (or DATABASE_URL / POSTGRES_URL) is required",
  );
}

const options: DiscordbotOptions = {
  activeExecutionTtlMs: optionalNumberEnv("DISCORDBOT_ACTIVE_EXECUTION_TTL_MS"),
  answerEditIntervalMs: optionalNumberEnv("DISCORDBOT_ANSWER_EDIT_INTERVAL_MS"),
  apiUrl,
  apiKey: optionalEnv("DISCORDBOT_API_KEY") ?? optionalEnv("CENTAUR_API_KEY"),
  applicationId,
  botToken,
  publicKey,
  discordApiUrl: optionalEnv("DISCORD_API_URL"),
  guildAllowlist: optionalList("DISCORDBOT_GUILD_ALLOWLIST"),
  idleTimeoutMs: optionalNumberEnv("SESSION_IDLE_TIMEOUT_MS"),
  isGatewayActive: () => gateway.isActive(),
  maxConcurrentExecutionsPerGuild: optionalNumberEnv(
    "DISCORDBOT_MAX_CONCURRENT_EXECUTIONS_PER_GUILD",
  ),
  maxDurationMs: optionalNumberEnv("SESSION_MAX_DURATION_MS"),
  mentionRoleIds: optionalList("DISCORD_MENTION_ROLE_IDS"),
  nameThreads: optionalEnv("DISCORDBOT_NAME_THREADS") !== "false",
  postgresUrl,
  stateKeyPrefix: optionalEnv("DISCORDBOT_STATE_KEY_PREFIX"),
  triggerBotAllowlist: optionalList("DISCORDBOT_TRIGGER_BOT_ALLOWLIST"),
  userName: stringEnv("DISCORDBOT_USER_NAME", "centaur"),
  logger: consoleLogger,
};

const { app, chat, adapter } = createDiscordbot(options);
const server = Bun.serve({ port, fetch: app.fetch });

log("info", "discordbot_started", {
  port: server.port,
  api_url: apiUrl,
});

const shutdown = async (signal: string): Promise<void> => {
  log("info", "discordbot_shutdown_started", { signal });
  await gateway.shutdown();
  await chat.shutdown().catch(() => undefined);
  server.stop();
  log("info", "discordbot_shutdown_complete", { signal });
  process.exit(0);
};
process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));

await gateway.start(chat, adapter);

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
      service: "discordbot",
      timestamp: new Date().toISOString(),
      event: message,
      ...(data && typeof data === "object"
        ? (data as Record<string, unknown>)
        : {}),
    }),
  );
}
