import { describe, expect, test } from "bun:test";
import { createMemoryState } from "@chat-adapter/state-memory";
import { createGithubbot, type GithubbotOptions } from "../src";

function options(overrides: Partial<GithubbotOptions> = {}): GithubbotOptions {
  return {
    apiUrl: "http://api-rs.local",
    connectStateOnStart: false,
    fetch: async () => new Response("ok"),
    readinessCheck: async () => undefined,
    state: createMemoryState(),
    token: "test-token",
    userName: "test-bot",
    webhookSecret: "test-secret",
    ...overrides,
  };
}

describe("githubbot health probes", () => {
  test("liveness does not depend on external services", async () => {
    const { app } = createGithubbot(
      options({
        fetch: async () => {
          throw new Error("unavailable");
        },
        readinessCheck: async () => {
          throw new Error("unavailable");
        },
      }),
    );

    expect((await app.request("/livez")).status).toBe(200);
  });

  test("readiness succeeds when Postgres and api-rs are ready", async () => {
    const { app } = createGithubbot(options());
    const response = await app.request("/readyz");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ ready: true, service: "githubbot" });
  });

  test("readiness fails when Postgres is unavailable", async () => {
    const { app } = createGithubbot(
      options({
        readinessCheck: async () => {
          throw new Error("database unavailable");
        },
      }),
    );
    const response = await app.request("/readyz");

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ ready: false, dependency: "postgres" });
  });

  test("readiness fails on an api-rs 5xx", async () => {
    const { app } = createGithubbot(
      options({ fetch: async () => new Response("unavailable", { status: 503 }) }),
    );
    const response = await app.request("/readyz");

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ ready: false, dependency: "api-rs" });
  });

  test("readiness bounds an unresponsive api-rs call", async () => {
    const { app } = createGithubbot(
      options({
        fetch: async (_input, init) =>
          new Promise((_resolve, reject) => {
            init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
          }),
        readinessTimeoutMs: 5,
      }),
    );
    const response = await app.request("/readyz");

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ ready: false, dependency: "api-rs" });
  });
});
