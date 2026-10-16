import { createTelegrambot, type TelegrambotOptions } from "./index";
import { loadTelegrambotConfig } from "./config";

const consoleLogger = {
  debug: (message: string, data?: unknown) => log("debug", message, data),
  info: (message: string, data?: unknown) => log("info", message, data),
  warn: (message: string, data?: unknown) => log("warn", message, data),
  error: (message: string, data?: unknown) => log("error", message, data),
  child: () => consoleLogger,
};

const config = loadTelegrambotConfig(process.env);
const options: TelegrambotOptions = {
  ...config.options,
  logger: consoleLogger,
};

const { adapter, app, chat } = createTelegrambot(options);
const server = Bun.serve({ port: config.port, fetch: app.fetch });

const shutdown = async (signal: string): Promise<void> => {
  log("info", "telegrambot_shutdown_started", { signal });
  // Stops the getUpdates loop (polling) after in-flight handlers settle.
  await chat.shutdown().catch(() => undefined);
  server.stop();
  log("info", "telegrambot_shutdown_complete", { signal });
  process.exit(0);
};
process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));

// Resolves the bot identity (getMe) and, in polling mode, deletes any
// registered webhook and starts the getUpdates loop.
await chat.initialize();
if (!adapter.botUserId) {
  // Without the bot's own id the self-message and reply-to-bot checks cannot
  // run; a bad token or blocked egress must crash-loop visibly instead.
  log("error", "telegrambot_identity_unresolved", {
    hint: "getMe failed: check TELEGRAM_BOT_TOKEN and egress to the Bot API.",
  });
  process.exit(1);
}

log("info", "telegrambot_started", {
  api_url: options.apiUrl,
  bot_user_id: adapter.botUserId,
  bot_user_name: adapter.userName,
  mode: adapter.runtimeMode,
  port: server.port,
});

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
