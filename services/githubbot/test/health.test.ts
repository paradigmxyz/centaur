import { createMemoryState } from "@chat-adapter/state-memory";
import { describe, expect, test } from "bun:test";
import { createGithubbot } from "../src/index";

const options = {
  apiUrl: "http://localhost:8080",
  botUserId: "42",
  connectStateOnStart: false,
  token: "test-token",
  userName: "github-bot",
  webhookSecret: "test-secret",
};

async function triggerInitialization(baseUrl: URL) {
  await fetch(new URL("/api/webhooks/github", baseUrl), {
    method: "POST",
    headers: { "x-github-event": "push" },
  });
}

async function status(baseUrl: URL, path: string) {
  return (await fetch(new URL(path, baseUrl))).status;
}

describe("githubbot health", () => {
  test("marks a poisoned Chat instance unhealthy until a fresh instance initializes", async () => {
    const state = createMemoryState();
    const connect = state.connect.bind(state);
    let databaseAvailable = false;
    state.connect = async () => {
      if (!databaseAvailable) throw new Error("postgres unavailable");
      await connect();
    };
    const failed = createGithubbot({ ...options, state });

    const failedServer = Bun.serve({ port: 0, fetch: failed.app.fetch });
    try {
      expect(await status(failedServer.url, "/health")).toBe(200);
      expect(await status(failedServer.url, "/ready")).toBe(503);

      await triggerInitialization(failedServer.url);
      expect(await status(failedServer.url, "/health")).toBe(503);
      expect(await status(failedServer.url, "/ready")).toBe(503);

      databaseAvailable = true;
      await triggerInitialization(failedServer.url);
      expect(await status(failedServer.url, "/health")).toBe(503);
    } finally {
      failedServer.stop(true);
    }

    const recovered = createGithubbot({ ...options, state: createMemoryState() });
    const recoveredServer = Bun.serve({ port: 0, fetch: recovered.app.fetch });
    try {
      await triggerInitialization(recoveredServer.url);
      expect(await status(recoveredServer.url, "/health")).toBe(200);
      expect(await status(recoveredServer.url, "/ready")).toBe(200);
    } finally {
      recoveredServer.stop(true);
    }
  });
});
