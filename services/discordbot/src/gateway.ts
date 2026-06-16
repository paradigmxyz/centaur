import type { Chat, Logger } from "chat";
import type { GatewayCapableAdapter } from "./types";

/**
 * `startGatewayListener` treats `durationMs` as a self-destruct timer backed by a single
 * `setTimeout`; within that window discord.js maintains one Gateway session with native RESUME,
 * so a large value gives us one long-lived connection rather than a re-IDENTIFY loop (which would
 * burn the 1000/24h IDENTIFY budget). If the connection ends before this elapses it's a
 * fatal/login error and we let the process exit so Kubernetes restarts the pod.
 *
 * This is capped at the maximum delay a 32-bit `setTimeout` can represent (2^31-1 ms ≈ 24.8 days).
 * A larger value (e.g. one year) silently overflows and clamps to 1ms, firing the self-destruct
 * almost immediately and crash-looping the pod. At ~24.8 days the timer forces at most one
 * reconnect/IDENTIFY per window — negligible against the 1000/24h budget.
 */
const LONG_RUNNING_MS = 2_147_483_647;

// Discord delta (no slackbotv2 analog): discord.js can sit in a RESUME loop
// for a long time without the listener promise settling, so `/health` also
// needs to reflect transient connection state. The adapter does not expose a
// status callback yet (see the TODO(wire) note at the adapter construction
// site); once wired, `setGatewayConnected` flips this timestamp and
// `isActive()` goes false after the gateway has been down for >60s.
const GATEWAY_DISCONNECT_STALE_MS = 60_000;

let gatewayDisconnectedAtMs: number | null = null;

/** Records a Gateway connect/disconnect transition (timestamp-based). */
export function setGatewayConnected(
  connected: boolean,
  atEpochMs = Date.now(),
): void {
  if (connected) {
    gatewayDisconnectedAtMs = null;
    return;
  }
  // Keep the FIRST disconnect timestamp so repeated disconnect signals
  // don't push the staleness window forward.
  gatewayDisconnectedAtMs ??= atEpochMs;
}

/** True until the gateway has been disconnected for more than the stale window. */
export function isGatewayConnectionFresh(nowEpochMs = Date.now()): boolean {
  return (
    gatewayDisconnectedAtMs === null ||
    nowEpochMs - gatewayDisconnectedAtMs <= GATEWAY_DISCONNECT_STALE_MS
  );
}

export type GatewayController = {
  /** True once the listener has started and the connection has not ended. */
  isActive(): boolean;
  /** Initialize the chat instance and open the single long-lived Gateway connection. */
  start(chat: Chat, adapter: GatewayCapableAdapter): Promise<void>;
  /** Stop accepting Gateway work and wait for the connection to close. */
  shutdown(): Promise<void>;
};

type GatewayControllerDeps = {
  logger: Logger;
  /** Override for tests — defaults to `process.exit`. */
  onFatalEnd?: () => void;
};

export function createGatewayController(
  deps: GatewayControllerDeps,
): GatewayController {
  const { logger } = deps;
  const onFatalEnd = deps.onFatalEnd ?? (() => process.exit(1));
  const abort = new AbortController();
  let active = false;
  let shuttingDown = false;
  let monitor: Promise<void> | undefined;

  return {
    isActive: () => active && isGatewayConnectionFresh(),

    async start(chat, adapter) {
      // Adapters initialize lazily (normally on the first webhook). Direct-mode Gateway
      // processing needs the adapter wired to the chat instance up front.
      await chat.initialize();

      const tracked: Array<Promise<unknown>> = [];
      // Direct mode: no webhookUrl, so MessageCreate is dispatched through Chat in-process.
      await adapter.startGatewayListener(
        {
          waitUntil: (promise) =>
            tracked.push(Promise.resolve(promise).catch(() => undefined)),
        },
        LONG_RUNNING_MS,
        abort.signal,
        undefined,
      );
      active = true;
      logger.info("discordbot_gateway_started");

      monitor = Promise.all(tracked)
        .then(() => undefined)
        .finally(() => {
          active = false;
          if (shuttingDown) {
            logger.info("discordbot_gateway_stopped");
            return;
          }
          // A single long-lived connection ended on its own — almost always a fatal error
          // (invalid token / disallowed intents). Exit so k8s restarts with backoff.
          logger.error("discordbot_gateway_ended_unexpectedly");
          onFatalEnd();
        });
    },

    async shutdown() {
      shuttingDown = true;
      abort.abort();
      if (monitor) await monitor;
    },
  };
}
