import { afterEach, describe, expect, it } from "bun:test";
import type { Chat, Logger } from "chat";
import {
  createGatewayController,
  isGatewayConnectionFresh,
  setGatewayConnected,
} from "../src/gateway";
import type { GatewayCapableAdapter } from "../src/types";

const silentLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => silentLogger,
};

const fakeChat = { initialize: async () => undefined } as unknown as Chat;

/**
 * Fake adapter mirroring `startGatewayListener`'s contract: it registers a long-lived promise
 * via `waitUntil` and resolves it when the abort signal fires (graceful stop).
 */
function fakeAdapter(): {
  adapter: GatewayCapableAdapter;
  endListener: () => void;
} {
  let endListener!: () => void;
  const listenerPromise = new Promise<void>((resolve) => {
    endListener = resolve;
  });
  const adapter: GatewayCapableAdapter = {
    async startGatewayListener(options, _durationMs, abortSignal) {
      abortSignal?.addEventListener("abort", () => endListener());
      options.waitUntil(listenerPromise);
      return new Response("ok");
    },
  };
  return { adapter, endListener };
}

describe("createGatewayController", () => {
  it("marks active once started", async () => {
    const { adapter } = fakeAdapter();
    const controller = createGatewayController({
      logger: silentLogger,
      onFatalEnd: () => undefined,
    });
    expect(controller.isActive()).toBe(false);
    await controller.start(fakeChat, adapter);
    expect(controller.isActive()).toBe(true);
  });

  it("does not treat a shutdown-triggered end as fatal", async () => {
    let fatal = false;
    const { adapter } = fakeAdapter();
    const controller = createGatewayController({
      logger: silentLogger,
      onFatalEnd: () => {
        fatal = true;
      },
    });
    await controller.start(fakeChat, adapter);
    await controller.shutdown();
    expect(controller.isActive()).toBe(false);
    expect(fatal).toBe(false);
  });

  it("treats an unexpected connection end as fatal", async () => {
    let fatal = false;
    const { adapter, endListener } = fakeAdapter();
    const controller = createGatewayController({
      logger: silentLogger,
      onFatalEnd: () => {
        fatal = true;
      },
    });
    await controller.start(fakeChat, adapter);
    endListener(); // connection dropped without a shutdown request
    await Bun.sleep(5);
    expect(fatal).toBe(true);
    expect(controller.isActive()).toBe(false);
  });
});

describe("gateway connection staleness", () => {
  afterEach(() => {
    // The disconnect timestamp is module-level; reset it between tests.
    setGatewayConnected(true);
  });

  it("stays fresh while connected", () => {
    setGatewayConnected(true);
    expect(isGatewayConnectionFresh()).toBe(true);
  });

  it("stays fresh within 60s of a disconnect", () => {
    const t0 = 1_000_000;
    setGatewayConnected(false, t0);
    expect(isGatewayConnectionFresh(t0 + 60_000)).toBe(true);
  });

  it("goes stale after the gateway has been down for more than 60s", () => {
    const t0 = 1_000_000;
    setGatewayConnected(false, t0);
    expect(isGatewayConnectionFresh(t0 + 60_001)).toBe(false);
  });

  it("keeps the FIRST disconnect timestamp across repeated disconnect signals", () => {
    const t0 = 1_000_000;
    setGatewayConnected(false, t0);
    setGatewayConnected(false, t0 + 59_000);
    expect(isGatewayConnectionFresh(t0 + 60_001)).toBe(false);
  });

  it("a reconnect flips it back to fresh", () => {
    const t0 = 1_000_000;
    setGatewayConnected(false, t0);
    expect(isGatewayConnectionFresh(t0 + 120_000)).toBe(false);
    setGatewayConnected(true);
    expect(isGatewayConnectionFresh(t0 + 120_000)).toBe(true);
  });

  it("makes the controller report inactive while stale", async () => {
    const { adapter } = fakeAdapter();
    const controller = createGatewayController({
      logger: silentLogger,
      onFatalEnd: () => undefined,
    });
    await controller.start(fakeChat, adapter);
    expect(controller.isActive()).toBe(true);
    setGatewayConnected(false, Date.now() - 120_000);
    expect(controller.isActive()).toBe(false);
    setGatewayConnected(true);
    expect(controller.isActive()).toBe(true);
  });
});
