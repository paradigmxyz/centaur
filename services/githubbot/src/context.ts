import { AsyncLocalStorage } from "node:async_hooks";

export type GithubbotRequestContext = {
  retryableErrors: unknown[];
  waitUntil(promise: Promise<unknown>): void;
};

type WaitUntilContext = {
  waitUntil(promise: Promise<unknown>): void;
};

export const requestContext = new AsyncLocalStorage<GithubbotRequestContext>();

/**
 * Attach a fire-and-forget promise to the in-flight request's keep-alive budget
 * when one is active, so the background turn outlives the webhook ack. Outside a
 * request (startup tasks) it just runs detached with a swallowed rejection.
 */
const inFlightBackgroundWork = new Set<Promise<unknown>>();

export function backgroundWaitUntil(promise: Promise<unknown>): void {
  // Track every background turn process-wide (independent of the per-request
  // keep-alive) so a graceful shutdown can drain in-flight work. Claims are taken
  // before the work runs, so a turn dropped at SIGTERM would be permanently
  // suppressed by its own claim on the inevitable webhook redelivery — draining
  // lets it finish instead.
  const tracked = promise.catch(() => undefined);
  inFlightBackgroundWork.add(tracked);
  void tracked.finally(() => inFlightBackgroundWork.delete(tracked));

  const context = requestContext.getStore();
  if (context) {
    context.waitUntil(promise);
  }
}

const sessionTails = new Map<string, Promise<void>>();

/**
 * Serialize async work by key: calls sharing a key run one at a time in arrival
 * order, while different keys run concurrently. Used to keep two turns targeting
 * the same session/sandbox (e.g. a conversational mention on an owned PR and that
 * PR's CI-fix turn, both keyed to `github-manage:…`) from interleaving git/push
 * operations. The bot runs a single replica, so an in-process queue suffices.
 */
export async function runExclusive<T>(
  key: string,
  fn: () => Promise<T>,
): Promise<T> {
  const prior = sessionTails.get(key) ?? Promise.resolve();
  let release!: () => void;
  const done = new Promise<void>((resolve) => {
    release = resolve;
  });
  const tail = prior.then(
    () => done,
    () => done,
  );
  sessionTails.set(key, tail);
  await prior.catch(() => undefined);
  try {
    return await fn();
  } finally {
    release();
    // Drop the entry once we're the last in line, so the map can't grow without
    // bound across the lifetime of the process.
    if (sessionTails.get(key) === tail) sessionTails.delete(key);
  }
}

/**
 * Await in-flight background turns up to a deadline so a SIGTERM (deploy/rollout)
 * lets running CI fixes, reviews, issue work, and merges finish instead of being
 * silently lost. Bounded so a hung or very long turn can't block shutdown past
 * the pod's termination grace period. Returns how many turns were in flight.
 */
export async function drainBackgroundWork(timeoutMs: number): Promise<number> {
  const pending = inFlightBackgroundWork.size;
  if (pending === 0) return 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, timeoutMs);
  });
  await Promise.race([
    Promise.allSettled([...inFlightBackgroundWork]),
    deadline,
  ]);
  if (timer) clearTimeout(timer);
  return pending;
}

export function waitUntil(
  c: { executionCtx: WaitUntilContext },
  promise: Promise<unknown>,
): void {
  try {
    c.executionCtx.waitUntil(promise);
  } catch {
    void promise.catch(() => undefined);
  }
}
