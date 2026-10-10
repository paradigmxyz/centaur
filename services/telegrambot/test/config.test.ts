import { describe, expect, it } from "bun:test";
import { loadTelegrambotConfig } from "../src/config";

const BASE = {
  TELEGRAM_BOT_TOKEN: "123:abc",
  TELEGRAMBOT_DATABASE_URL: "postgres://localhost/test",
};

describe("loadTelegrambotConfig", () => {
  it("defaults to polling with fail-closed empty allowlists", () => {
    const { options, port } = loadTelegrambotConfig(BASE);
    expect(port).toBe(3001);
    expect(options.mode).toBe("polling");
    expect(options.chatAllowlist).toEqual([]);
    expect(options.userAllowlist).toEqual([]);
    expect(options.webhookSecretToken).toBeUndefined();
  });

  it("parses allowlists and rejects non-numeric ids", () => {
    const { options } = loadTelegrambotConfig({
      ...BASE,
      TELEGRAMBOT_CHAT_ALLOWLIST: "-1001, -1002",
      TELEGRAMBOT_USER_ALLOWLIST: "42 43",
    });
    expect(options.chatAllowlist).toEqual(["-1001", "-1002"]);
    expect(options.userAllowlist).toEqual(["42", "43"]);
    expect(() =>
      loadTelegrambotConfig({ ...BASE, TELEGRAMBOT_USER_ALLOWLIST: "@ada" }),
    ).toThrow("TELEGRAMBOT_USER_ALLOWLIST must contain numeric Telegram ids");
  });

  it("requires a secret token in webhook mode", () => {
    expect(() =>
      loadTelegrambotConfig({ ...BASE, TELEGRAMBOT_MODE: "webhook" }),
    ).toThrow("TELEGRAM_WEBHOOK_SECRET_TOKEN is required");
    const { options } = loadTelegrambotConfig({
      ...BASE,
      TELEGRAMBOT_MODE: "webhook",
      TELEGRAM_WEBHOOK_SECRET_TOKEN: "s3cret",
    });
    expect(options.mode).toBe("webhook");
  });

  it("rejects unknown modes and missing required settings", () => {
    expect(() =>
      loadTelegrambotConfig({ ...BASE, TELEGRAMBOT_MODE: "auto" }),
    ).toThrow("TELEGRAMBOT_MODE");
    expect(() =>
      loadTelegrambotConfig({ TELEGRAMBOT_DATABASE_URL: "postgres://x" }),
    ).toThrow("TELEGRAM_BOT_TOKEN is required");
    expect(() => loadTelegrambotConfig({ TELEGRAM_BOT_TOKEN: "1:a" })).toThrow(
      "TELEGRAMBOT_DATABASE_URL",
    );
    expect(() => loadTelegrambotConfig({ ...BASE, PORT: "0" })).toThrow(
      "PORT must be a positive integer",
    );
  });
});
