// Chrome DevTools Protocol over `ctx.websocket`, for connectors that drive a
// remote browser. A connection lasts one connector call (the runtime closes
// the socket when the call ends), so every function connects, attaches to a
// page, does its work and closes; the browser keeps its state between calls.
import type { Ctx, WebSocketConn } from "../centaur";

/** How long one command waits for its reply by default. */
export const COMMAND_TIMEOUT_MS: number = 30000;
/** Events kept while waiting for replies; older ones are dropped. */
const MAX_EVENTS = 500;

/** A CDP event (`Page.loadEventFired`, ...). */
export interface CdpEvent {
  method: string;
  params: any;
  sessionId?: string;
}

/** A page target as `Target.getTargets` lists it. */
export interface PageTarget {
  targetId: string;
  type: string;
  title: string;
  url: string;
  attached?: boolean;
  [field: string]: unknown;
}

/** One browser-level CDP connection. */
export class Cdp {
  private socket: WebSocketConn;
  private nextId: number = 1;
  private events: CdpEvent[] = [];

  constructor(socket: WebSocketConn) {
    this.socket = socket;
  }

  /** Send a command (to a page when `sessionId` is set) and return its `result`. */
  async send(
    method: string,
    params: Record<string, unknown> = {},
    sessionId?: string,
    timeout_ms: number = COMMAND_TIMEOUT_MS,
  ): Promise<any> {
    const id = this.nextId++;
    const message: Record<string, unknown> = { id, method, params };
    if (sessionId) message.sessionId = sessionId;
    await this.socket.send(JSON.stringify(message));
    const deadline = Date.now() + timeout_ms;
    for (;;) {
      const left = deadline - Date.now();
      const raw = left > 0 ? await this.socket.recv(left) : null;
      if (raw === null) throw new Error(`CDP ${method} got no reply within ${timeout_ms} ms`);
      const reply = this.parse(raw);
      if (reply === undefined) continue;
      if (reply.id === id) {
        if (reply.error) {
          throw new Error(`CDP ${method} failed: ${reply.error.message ?? JSON.stringify(reply.error)}`);
        }
        return reply.result ?? {};
      }
    }
  }

  /** Forget the events seen so far, before a command whose events matter. */
  clearEvents(): void {
    this.events = [];
  }

  /**
   * The first event named `method` (for `sessionId`, when set) since the last `clearEvents`,
   * waiting up to `timeout_ms`; null when none arrives.
   */
  async waitFor(method: string, sessionId: string | undefined, timeout_ms: number): Promise<CdpEvent | null> {
    const matches = (event: CdpEvent): boolean =>
      event.method === method && (!sessionId || event.sessionId === sessionId);
    const deadline = Date.now() + timeout_ms;
    for (;;) {
      const index = this.events.findIndex(matches);
      if (index >= 0) return this.events.splice(index, 1)[0];
      const left = deadline - Date.now();
      if (left <= 0) return null;
      const raw = await this.socket.recv(left);
      if (raw === null) return null;
      this.parse(raw);
    }
  }

  /** The page targets, in the browser's order. */
  async pages(): Promise<PageTarget[]> {
    const { targetInfos } = await this.send("Target.getTargets");
    return (targetInfos as PageTarget[]).filter((target) => target.type === "page");
  }

  /**
   * Attach to `target_id`, or to the first page (opening a blank one when there is none), in
   * flat mode, and return the page session to send page commands to.
   */
  async attach(target_id?: string): Promise<{ sessionId: string; target: PageTarget }> {
    const pages = await this.pages();
    let target = target_id ? pages.find((page) => page.targetId === target_id) : pages[0];
    if (target_id && !target) throw new Error(`no page with target_id ${target_id}`);
    if (!target) {
      const { targetId } = await this.send("Target.createTarget", { url: "about:blank" });
      target = { targetId, type: "page", title: "", url: "about:blank" };
    }
    const { sessionId } = await this.send("Target.attachToTarget", { targetId: target.targetId, flatten: true });
    return { sessionId, target };
  }

  async close(): Promise<void> {
    try {
      await this.socket.close();
    } catch {
      // already closed
    }
  }

  /** A reply, or undefined after buffering an event. */
  private parse(raw: string | Uint8Array): any {
    // CDP speaks JSON text frames.
    if (typeof raw !== "string") throw new Error("CDP sent a binary message");
    const message = JSON.parse(raw);
    if (message.id !== undefined) return message;
    if (typeof message.method === "string") {
      this.events.push({ method: message.method, params: message.params, sessionId: message.sessionId });
      if (this.events.length > MAX_EVENTS) this.events.splice(0, this.events.length - MAX_EVENTS);
    }
    return undefined;
  }
}

/**
 * Connect to a browser's CDP endpoint: a `ws(s)://` URL as is, or an `http(s)://` one through its
 * `/json/version` (`webSocketDebuggerUrl`), keeping any query string on the lookup.
 */
export async function connect(ctx: Ctx, endpoint: string): Promise<Cdp> {
  let url = endpoint;
  if (/^https?:\/\//i.test(endpoint)) {
    const [base, query] = splitQuery(endpoint);
    const res = await ctx.fetch(`${base.replace(/\/+$/, "")}/json/version${query}`, {
      headers: { accept: "application/json" },
    });
    const text = await res.text();
    if (!res.ok) throw new Error(`CDP endpoint returned HTTP ${res.status}: ${text.slice(0, 300)}`);
    let version: any;
    try {
      version = JSON.parse(text);
    } catch {
      throw new Error(`CDP endpoint returned non-JSON: ${text.slice(0, 300)}`);
    }
    if (typeof version.webSocketDebuggerUrl !== "string") {
      throw new Error("CDP endpoint did not return a webSocketDebuggerUrl");
    }
    url = version.webSocketDebuggerUrl;
  }
  return new Cdp(await ctx.websocket(url));
}

function splitQuery(url: string): [string, string] {
  const at = url.indexOf("?");
  return at < 0 ? [url, ""] : [url.slice(0, at), url.slice(at)];
}

/** `Runtime.evaluate` by value in a page; throws the page's exception. */
export async function evaluate(
  cdp: Cdp,
  sessionId: string,
  expression: string,
  options: { await_promise?: boolean; timeout_ms?: number } = {},
): Promise<unknown> {
  const reply = await cdp.send(
    "Runtime.evaluate",
    {
      expression,
      returnByValue: true,
      awaitPromise: options.await_promise ?? true,
      userGesture: true,
    },
    sessionId,
    options.timeout_ms ?? COMMAND_TIMEOUT_MS,
  );
  if (reply.exceptionDetails) {
    const details = reply.exceptionDetails;
    const description = details.exception?.description ?? details.exception?.value ?? details.text;
    throw new Error(`page threw: ${String(description).slice(0, 2000)}`);
  }
  return reply.result?.value;
}
