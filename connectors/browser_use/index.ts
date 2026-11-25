import type { Ctx } from "../centaur";
import { type Cdp, connect, evaluate, type PageTarget } from "../_lib/cdp";
import { outputPath, type SavedFile, saveBytes } from "../_lib/files";

// Browser Use (browser-use.com): cloud browsers this connector drives over the
// Chrome DevTools Protocol, plus Browser Use's LLM gateway and web search.
//
// A browser outlives calls and cells; a CDP connection does not. Each page
// function reconnects from the browser ID, attaches to a tab, does one thing
// and disconnects, so tabs, cookies and navigation persist between calls.
// The browser's CDP URL grants control without the API key, so it never
// leaves this connector; the browser ID is its hostname, so treat the ID (and
// `liveUrl`) as a secret capability too, and stop browsers when done: they
// bill until they stop or time out.

const DEFAULT_ORIGIN = "https://api.browser-use.com";
const BROWSERS = "/api/v4/browsers";
const LLM = "/api/v4/llm/v1";
const SEARCH = "/api/v4/search";
const DEFAULT_MODEL = "bu-4.0";
const DEFAULT_MAX_CHARS = 1_000_000;
const DEFAULT_NAVIGATE_TIMEOUT_MS = 30000;
// A connector call has 60 s; leave room to connect and read the result.
const MAX_WAIT_MS = 45000;
// The gateway cuts a non-streamed request at 300 s.
const DEFAULT_LLM_TIMEOUT_MS = 120000;
const MAX_LLM_TIMEOUT_MS = 300000;

/** A Browser Use cloud browser, as the API describes it, without its `cdpUrl`. */
export interface Browser {
  id: string;
  /** `active` or `stopped`. */
  status: string;
  /** Live view of the browser; anyone with it can watch and control the browser. */
  liveUrl?: string | null;
  timeoutAt: string;
  startedAt: string;
  finishedAt?: string | null;
  proxyUsedMb?: string;
  proxyCost?: string;
  browserCost?: string;
  recordingUrl?: string | null;
  metadata?: Record<string, string>;
  [field: string]: unknown;
}

/** Where a page function ran. */
export interface PageState {
  target_id: string;
  url: string;
  title: string;
}

/** One model the LLM gateway serves. */
export interface Model {
  id: string;
  object?: string;
  owned_by?: string;
  [field: string]: unknown;
}

/** An OpenAI Responses API response from the gateway, as sent. */
export interface ModelResponse {
  id: string;
  status?: string;
  model?: string;
  output?: unknown[];
  usage?: Record<string, unknown>;
  /** `{ resolved_model, service_tier, charge_usd }`. */
  browser_use?: Record<string, unknown>;
  [field: string]: unknown;
}

/** A web search result, as Browser Use sends it. */
export interface SearchResult {
  query?: string;
  count?: number;
  /** One text entry per hit (title, URL, excerpts), or the hits as one text. */
  results?: unknown;
  /** `{ charge_usd }`. */
  browser_use?: Record<string, unknown>;
  [field: string]: unknown;
}

function origin(ctx: Ctx): string {
  return (ctx.env.BROWSER_USE_API_ORIGIN || DEFAULT_ORIGIN).replace(/\/+$/, "");
}

function headers(ctx: Ctx, json: boolean): Record<string, string> {
  const value: Record<string, string> = { accept: "application/json" };
  if (json) value["content-type"] = "application/json";
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.BROWSER_USE_API_KEY) value["X-Browser-Use-API-Key"] = ctx.env.BROWSER_USE_API_KEY;
  return value;
}

/** Browser Use errors: `{ detail }` (REST) or `{ error: { code, message } }` (gateway). */
function errorDetail(payload: any, text: string): string {
  const error = payload?.error;
  if (error && typeof error === "object") {
    const message = error.message || error.code;
    if (typeof message === "string" && message) return message;
  }
  const detail = payload?.detail;
  if (typeof detail === "string" && detail) return detail;
  if (detail !== undefined) return JSON.stringify(detail).slice(0, 1000);
  return text.trim().slice(0, 1000);
}

async function call(
  ctx: Ctx,
  path: string,
  init: { method?: string; body?: unknown; timeout_ms?: number } = {},
): Promise<any> {
  const res = await ctx.fetch(`${origin(ctx)}${path}`, {
    method: init.method || "GET",
    headers: headers(ctx, init.body !== undefined),
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
    timeout_ms: init.timeout_ms,
  });
  const text = await res.text();
  let payload: any;
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    payload = undefined;
  }
  if (!res.ok) {
    throw new Error(`Browser Use returned HTTP ${res.status}: ${errorDetail(payload, text)}`);
  }
  if (payload === undefined || payload === null) {
    throw new Error(`Browser Use returned non-JSON: ${text.slice(0, 1000)}`);
  }
  return payload;
}

function browserId(args: { browser_id: string }): string {
  const id = (args.browser_id || "").trim();
  if (!id) throw new Error("browser_id is required");
  return encodeURIComponent(id);
}

/** The API's view of a browser, minus `cdpUrl`, which grants control without the key. */
function publicBrowser(raw: any): Browser {
  const { cdpUrl: _cdp, ...rest } = raw ?? {};
  return rest as Browser;
}

/** A fresh CDP connection to an active browser. */
async function connectBrowser(ctx: Ctx, args: { browser_id: string }): Promise<Cdp> {
  const raw = await call(ctx, `${BROWSERS}/${browserId(args)}`);
  if (raw.status !== "active" || typeof raw.cdpUrl !== "string" || !raw.cdpUrl) {
    throw new Error(`browser ${raw.id ?? args.browser_id} is ${raw.status ?? "unavailable"}, not active`);
  }
  return connect(ctx, raw.cdpUrl);
}

/** Run `fn` on one tab (or iframe target) of a browser over a fresh CDP connection. */
async function withPage<T>(
  ctx: Ctx,
  args: { browser_id: string; target_id?: string },
  fn: (cdp: Cdp, sessionId: string, target: PageTarget) => Promise<T>,
): Promise<T> {
  const cdp = await connectBrowser(ctx, args);
  try {
    const { sessionId, target } = await cdp.attach(args.target_id);
    return await fn(cdp, sessionId, target);
  } finally {
    await cdp.close();
  }
}

/** The tab's URL and title; while a navigation is replacing the page, what the target last reported. */
async function pageState(cdp: Cdp, sessionId: string, target: PageTarget): Promise<PageState> {
  try {
    const state = (await evaluate(cdp, sessionId, "({ url: location.href, title: document.title })")) as {
      url: string;
      title: string;
    };
    return { target_id: target.targetId, url: state.url, title: state.title };
  } catch {
    return { target_id: target.targetId, url: target.url, title: target.title };
  }
}

function waitBudget(timeout_ms: number | undefined, fallback: number): number {
  return Math.min(Math.max(timeout_ms ?? fallback, 0), MAX_WAIT_MS);
}

/** Poll `check` every 250 ms until it is truthy or `timeout_ms` passes; page errors count as not yet. */
async function poll(ctx: Ctx, cdp: Cdp, sessionId: string, check: string, timeout_ms: number): Promise<boolean> {
  const deadline = Date.now() + timeout_ms;
  for (;;) {
    try {
      if (await evaluate(cdp, sessionId, check)) return true;
    } catch {
      // the page is navigating; try again
    }
    if (Date.now() >= deadline) return false;
    await ctx.sleep(250);
  }
}

function clip(text: string, max_chars: number | undefined): { text: string; chars: number; truncated: boolean } {
  const limit = max_chars ?? DEFAULT_MAX_CHARS;
  const truncated = text.length > limit;
  return { text: truncated ? text.slice(0, limit) : text, chars: text.length, truncated };
}

/** An expression for one element, throwing a clear error when the selector matches nothing. */
function elementExpression(selector: string): string {
  const quoted = JSON.stringify(selector);
  return `(() => { const el = document.querySelector(${quoted}); ` +
    `if (!el) throw new Error("no element matches " + ${quoted}); return el; })()`;
}

/**
 * Navigate and poll until the new document reaches `wait_until`. The old document is marked
 * first, so a poll that lands on it before the new one commits does not count.
 */
async function navigateTo(
  ctx: Ctx,
  cdp: Cdp,
  sessionId: string,
  url: string,
  wait_until: "load" | "domcontentloaded" | "none",
  timeout_ms: number,
): Promise<boolean> {
  await evaluate(cdp, sessionId, "window.__centaurPrevious = true").catch(() => undefined);
  const result = await cdp.send("Page.navigate", { url }, sessionId, timeout_ms);
  if (result.errorText) throw new Error(`navigation to ${url} failed: ${result.errorText}`);
  // No loaderId: a same-document (fragment) navigation, already done.
  if (wait_until === "none" || !result.loaderId) return true;
  const ready = wait_until === "load" ? `document.readyState === "complete"` : `document.readyState !== "loading"`;
  return poll(ctx, cdp, sessionId, `!window.__centaurPrevious && ${ready}`, timeout_ms);
}

const HTML_EXPRESSION = `(() => {
  const doctype = document.doctype ? new XMLSerializer().serializeToString(document.doctype) + "\\n" : "";
  return doctype + document.documentElement.outerHTML;
})()`;

async function readHtml(
  ctx: Ctx,
  cdp: Cdp,
  sessionId: string,
  target: PageTarget,
  args: { selector?: string; path?: string; max_chars?: number },
): Promise<{ page: PageState; html?: string; path?: string; bytes?: number; chars: number; truncated: boolean }> {
  const expression = args.selector ? `${elementExpression(args.selector)}.outerHTML` : HTML_EXPRESSION;
  const html = String(await evaluate(cdp, sessionId, expression));
  const page = await pageState(cdp, sessionId, target);
  if (args.path !== undefined) {
    const saved = await saveBytes(ctx, outputPath(args.path || undefined, "browser_use", "page.html"), html, "browser_use.get_html", "text/html");
    return { page, path: saved.path, bytes: saved.bytes, chars: html.length, truncated: false };
  }
  const clipped = clip(html, args.max_chars);
  return { page, html: clipped.text, chars: clipped.chars, truncated: clipped.truncated };
}

// ---------------------------------------------------------------- browsers

/**
 * Start a cloud browser and return it (`id`, `status`, `liveUrl`, `timeoutAt`, costs). It bills
 * per minute until `stop_browser` or `timeout_minutes` (default 15, at most 240). `profile_id`
 * reuses a stored Browser Use profile (cookies, logins); `proxy_country_code` picks the proxy
 * exit (default `us`; `null` for none); `network_policy_id` applies a Browser Use network policy
 * that allows or blocks domains; `start_url` navigates the first tab once it is up (`start`
 * reports whether it loaded; the browser is returned and running either way).
 */
export async function create_browser(
  args: {
    profile_id?: string;
    proxy_country_code?: string | null;
    timeout_minutes?: number;
    screen_width?: number;
    screen_height?: number;
    network_policy_id?: string;
    solve_captchas?: boolean;
    enable_recording?: boolean;
    metadata?: Record<string, string>;
    start_url?: string;
  },
  ctx: Ctx,
): Promise<Browser & { start?: { loaded: boolean; error?: string } }> {
  const body: Record<string, unknown> = { timeout: args.timeout_minutes ?? 15 };
  if (args.profile_id) body.profileId = args.profile_id;
  if (args.proxy_country_code !== undefined) body.proxyCountryCode = args.proxy_country_code;
  if (args.screen_width !== undefined) body.browserScreenWidth = args.screen_width;
  if (args.screen_height !== undefined) body.browserScreenHeight = args.screen_height;
  if (args.network_policy_id) body.networkPolicyId = args.network_policy_id;
  if (args.solve_captchas !== undefined) body.solveCaptchas = args.solve_captchas;
  if (args.enable_recording !== undefined) body.enableRecording = args.enable_recording;
  if (args.metadata) body.metadata = args.metadata;
  const raw = await call(ctx, BROWSERS, { method: "POST", body });
  const browser = publicBrowser(raw);
  if (!args.start_url) return browser;
  // The browser is billing now: report a failed start rather than lose its ID.
  try {
    if (typeof raw.cdpUrl !== "string") throw new Error("Browser Use returned no CDP endpoint");
    const cdp = await connect(ctx, raw.cdpUrl);
    try {
      const { sessionId } = await cdp.attach();
      const loaded = await navigateTo(ctx, cdp, sessionId, args.start_url, "domcontentloaded", 20000);
      return { ...browser, start: { loaded } };
    } finally {
      await cdp.close();
    }
  } catch (error) {
    return { ...browser, start: { loaded: false, error: String(error) } };
  }
}

/** A browser's current state, costs and (once stopped, if enabled) `recordingUrl`. */
export async function get_browser(args: { browser_id: string }, ctx: Ctx): Promise<Browser> {
  return publicBrowser(await call(ctx, `${BROWSERS}/${browserId(args)}`));
}

/** Stop a browser, ending its billing (unused time is refunded) and saving its profile. */
export async function stop_browser(args: { browser_id: string }, ctx: Ctx): Promise<Browser> {
  return publicBrowser(
    await call(ctx, `${BROWSERS}/${browserId(args)}`, { method: "PATCH", body: { action: "stop" } }),
  );
}

// ------------------------------------------------------------------- pages

/** A tab, or a cross-origin iframe the browser runs as its own target. */
export interface Target {
  target_id: string;
  /** `page` (a tab) or `iframe`. */
  type: string;
  url: string;
  title: string;
}

/**
 * The browser's tabs and its out-of-process iframes, in the browser's order. A cross-origin
 * iframe's document is out of reach from its tab (`get_html` and `evaluate_js` on the tab see
 * only the `<iframe>` element), so pass an iframe's `target_id` to `evaluate_js`, `get_html` or
 * `click` to run inside it. Iframe targets come and go as frames load; list them again after
 * navigating. `screenshot` works only on tabs, and `click_at` and `scroll_at` coordinates come
 * from tab screenshots, so use those on the tab.
 */
export async function list_targets(args: { browser_id: string }, ctx: Ctx): Promise<Target[]> {
  const cdp = await connectBrowser(ctx, args);
  try {
    return (await cdp.targets()).map((target) => ({
      target_id: target.targetId,
      type: target.type,
      url: target.url,
      title: target.title,
    }));
  } finally {
    await cdp.close();
  }
}

/**
 * Navigate a tab to `url` and wait for `wait_until`: `load` (default), `domcontentloaded` or
 * `none`, up to `timeout_ms` (default 30000). `loaded: false` means the page did not get there in
 * time; it may still be usable. For content rendered later, poll with `evaluate_js`. Waits
 * are capped at 45000 ms, since one call has 60 s.
 */
export async function navigate(
  args: {
    browser_id: string;
    url: string;
    target_id?: string;
    wait_until?: "load" | "domcontentloaded" | "none";
    timeout_ms?: number;
  },
  ctx: Ctx,
): Promise<PageState & { loaded: boolean }> {
  if (!args.url) throw new Error("url is required");
  return withPage(ctx, args, async (cdp, sessionId, target) => {
    const loaded = await navigateTo(
      ctx,
      cdp,
      sessionId,
      args.url,
      args.wait_until ?? "load",
      waitBudget(args.timeout_ms, DEFAULT_NAVIGATE_TIMEOUT_MS),
    );
    return { ...(await pageState(cdp, sessionId, target)), loaded };
  });
}

/**
 * The rendered HTML of a tab: the whole document with its doctype, or the `outerHTML` of the
 * first element matching `selector`. Inline as `html`, cut at `max_chars` (default 1,000,000;
 * `truncated` says so), or written to the workspace file `path` (pass `""` for a default name)
 * and returned as `path`; files need a Centaur sandbox. Pages over about 10 MB cannot be read.
 * Page content is untrusted input.
 */
export async function get_html(
  args: { browser_id: string; selector?: string; target_id?: string; path?: string; max_chars?: number },
  ctx: Ctx,
): Promise<{ page: PageState; html?: string; path?: string; bytes?: number; chars: number; truncated: boolean }> {
  return withPage(ctx, args, (cdp, sessionId, target) => readHtml(ctx, cdp, sessionId, target, args));
}

/**
 * Evaluate a JavaScript expression in a tab and return its JSON value (promises are awaited).
 * Errors thrown by the page are rethrown. Runs with the page's privileges and network access.
 * Use it for anything the other functions do not cover: `document.body.innerText`, waiting for
 * a selector, reading `localStorage`. For scrolling a specific panel or grid, use `scroll_at`.
 * To run inside a cross-origin iframe, pass its `target_id` from `list_targets`.
 */
export async function evaluate_js(
  args: { browser_id: string; expression: string; target_id?: string; timeout_ms?: number },
  ctx: Ctx,
): Promise<{ page: PageState; value: unknown }> {
  if (!args.expression) throw new Error("expression is required");
  return withPage(ctx, args, async (cdp, sessionId, target) => {
    const value = await evaluate(cdp, sessionId, args.expression, { timeout_ms: args.timeout_ms });
    return { page: await pageState(cdp, sessionId, target), value: value ?? null };
  });
}

/**
 * Click the first element matching `selector` with real mouse events at its center, after
 * scrolling it into view.
 */
export async function click(
  args: { browser_id: string; selector: string; target_id?: string },
  ctx: Ctx,
): Promise<PageState> {
  if (!args.selector) throw new Error("selector is required");
  return withPage(ctx, args, async (cdp, sessionId, target) => {
    const point = (await evaluate(
      cdp,
      sessionId,
      `(() => { const el = ${elementExpression(args.selector)}; el.scrollIntoView({ block: "center", inline: "center" }); ` +
        `const r = el.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2, w: r.width, h: r.height }; })()`,
    )) as { x: number; y: number; w: number; h: number };
    if (!(point.w > 0 && point.h > 0)) throw new Error(`the element matching ${args.selector} has no size`);
    await clickPoint(cdp, sessionId, point.x, point.y);
    return pageState(cdp, sessionId, target);
  });
}

function coordinate(name: string, value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) throw new Error(`${name} must be a finite number`);
  return value;
}

/** A left click with real mouse events at viewport point (`x`, `y`). */
async function clickPoint(cdp: Cdp, sessionId: string, x: number, y: number): Promise<void> {
  const mouse = (type: string): Promise<unknown> =>
    cdp.send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 }, sessionId);
  await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y }, sessionId);
  await mouse("mousePressed");
  await mouse("mouseReleased");
}

/**
 * Click with real mouse events at viewport point (`x`, `y`) in CSS pixels, from the top-left of
 * the visible viewport, as read off a (non-`full_page`) `screenshot`. Use it for targets with no
 * usable selector, such as canvas or virtualized grids. If the screenshot is larger than the
 * viewport (device pixel ratio above 1), divide its pixel coordinates by that ratio.
 */
export async function click_at(
  args: { browser_id: string; x: number; y: number; target_id?: string },
  ctx: Ctx,
): Promise<PageState> {
  const x = coordinate("x", args.x);
  const y = coordinate("y", args.y);
  return withPage(ctx, args, async (cdp, sessionId, target) => {
    await clickPoint(cdp, sessionId, x, y);
    return pageState(cdp, sessionId, target);
  });
}

/**
 * Scroll with a mouse-wheel event at viewport point (`x`, `y`) in CSS pixels, so the element
 * under the pointer (a scrollable grid or panel, not only the page) scrolls. `delta_y` scrolls
 * down when positive, `delta_x` right; both in CSS pixels, default 0. Returns once the event is
 * dispatched; take a screenshot to see the result.
 */
export async function scroll_at(
  args: { browser_id: string; x: number; y: number; delta_y?: number; delta_x?: number; target_id?: string },
  ctx: Ctx,
): Promise<PageState> {
  const x = coordinate("x", args.x);
  const y = coordinate("y", args.y);
  const deltaY = coordinate("delta_y", args.delta_y ?? 0);
  const deltaX = coordinate("delta_x", args.delta_x ?? 0);
  if (deltaX === 0 && deltaY === 0) throw new Error("delta_y or delta_x is required");
  return withPage(ctx, args, async (cdp, sessionId, target) => {
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y }, sessionId);
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX, deltaY }, sessionId);
    return pageState(cdp, sessionId, target);
  });
}

/**
 * Type `text` into the first element matching `selector` (or the focused element). `clear`
 * empties an input or textarea first; `submit` presses Enter afterwards.
 */
export async function type_text(
  args: { browser_id: string; text: string; selector?: string; clear?: boolean; submit?: boolean; target_id?: string },
  ctx: Ctx,
): Promise<PageState> {
  return withPage(ctx, args, async (cdp, sessionId, target) => {
    if (args.selector) {
      await evaluate(
        cdp,
        sessionId,
        `(() => { const el = ${elementExpression(args.selector)}; el.scrollIntoView({ block: "center" }); el.focus(); ` +
          (args.clear
            ? `if ("value" in el) { el.value = ""; el.dispatchEvent(new Event("input", { bubbles: true })); } `
            : "") +
          `return true; })()`,
      );
    }
    if (args.text) await cdp.send("Input.insertText", { text: args.text }, sessionId);
    if (args.submit) {
      const key = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13 };
      await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", text: "\r", ...key }, sessionId);
      await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...key }, sessionId);
    }
    return pageState(cdp, sessionId, target);
  });
}

/**
 * Save a screenshot of a tab to the workspace (`path`, default `downloads/browser_use/...`) as
 * `png` (default) or `jpeg`; `full_page` captures beyond the viewport. Tabs only: an iframe
 * target cannot be captured, but it shows in its tab's screenshot. Needs a Centaur sandbox.
 *
 * @requires files
 */
export async function screenshot(
  args: { browser_id: string; path?: string; full_page?: boolean; format?: "png" | "jpeg"; quality?: number; target_id?: string },
  ctx: Ctx,
): Promise<{ page: PageState; file: SavedFile }> {
  const format = args.format ?? "png";
  return withPage(ctx, args, async (cdp, sessionId, target) => {
    const params: Record<string, unknown> = { format };
    if (format === "jpeg" && args.quality !== undefined) params.quality = args.quality;
    if (args.full_page) {
      const metrics = await cdp.send("Page.getLayoutMetrics", {}, sessionId);
      const size = metrics.cssContentSize ?? metrics.contentSize;
      params.captureBeyondViewport = true;
      params.clip = { x: 0, y: 0, width: size.width, height: size.height, scale: 1 };
    }
    const { data } = await cdp.send("Page.captureScreenshot", params, sessionId, 60000);
    const page = await pageState(cdp, sessionId, target);
    const name = `screenshot-${Date.now()}.${format === "jpeg" ? "jpg" : "png"}`;
    const file = await saveBytes(
      ctx,
      outputPath(args.path, "browser_use", name),
      ctx.base64.decode(data),
      "browser_use.screenshot",
      format === "jpeg" ? "image/jpeg" : "image/png",
    );
    return { page, file };
  });
}

/**
 * One call from URL to rendered HTML: start a browser, load `url`, read the HTML (as `get_html`:
 * whole document or `selector`, inline up to `max_chars` or to `path`), and stop the browser,
 * even on failure. `wait_for_selector` waits (up to 10 s) for content rendered after load. The
 * whole run must fit in one 60 s call; if it does not, the browser stops on its own after 5
 * minutes. Use the browser functions for slow or multi-step pages.
 */
export async function fetch_rendered_html(
  args: {
    url: string;
    selector?: string;
    wait_for_selector?: string;
    path?: string;
    max_chars?: number;
    profile_id?: string;
    proxy_country_code?: string | null;
    network_policy_id?: string;
  },
  ctx: Ctx,
): Promise<{ browser_id: string; page: PageState; html?: string; path?: string; bytes?: number; chars: number; truncated: boolean }> {
  if (!args.url) throw new Error("url is required");
  const body: Record<string, unknown> = { timeout: 5 };
  if (args.profile_id) body.profileId = args.profile_id;
  if (args.proxy_country_code !== undefined) body.proxyCountryCode = args.proxy_country_code;
  if (args.network_policy_id) body.networkPolicyId = args.network_policy_id;
  const raw = await call(ctx, BROWSERS, { method: "POST", body });
  const id: string = raw.id;
  try {
    if (typeof raw.cdpUrl !== "string") throw new Error("Browser Use returned no CDP endpoint for the browser");
    const cdp = await connect(ctx, raw.cdpUrl);
    try {
      const { sessionId, target } = await cdp.attach();
      await navigateTo(ctx, cdp, sessionId, args.url, "load", 25000);
      if (args.wait_for_selector) {
        const check = `document.querySelector(${JSON.stringify(args.wait_for_selector)}) !== null`;
        await poll(ctx, cdp, sessionId, check, 10000);
      }
      return { browser_id: id, ...(await readHtml(ctx, cdp, sessionId, target, args)) };
    } finally {
      await cdp.close();
    }
  } finally {
    await call(ctx, `${BROWSERS}/${encodeURIComponent(id)}`, { method: "PATCH", body: { action: "stop" } }).catch(
      () => undefined,
    );
  }
}

// --------------------------------------------------------------------- LLM

/** Models the Browser Use LLM gateway serves to this key (`bu-4.0`, `bu-4.0-mini`, ...). Free. */
export async function llm_models(args: {}, ctx: Ctx): Promise<{ data: Model[]; [field: string]: unknown }> {
  return call(ctx, `${LLM}/models`);
}

/**
 * One OpenAI Responses API request to the Browser Use LLM gateway, returned as
 * `{ output_text, response }`: the concatenated output text and the response as sent (with
 * `usage` and `browser_use.charge_usd`). `input` is a string or a Responses input array;
 * `model` defaults to `bu-4.0` (see `llm_models`). `json_schema` asks for structured output
 * (then `output_text` is JSON text). `options` adds any other Responses API fields
 * (`max_output_tokens`, `reasoning: { effort }`, `tools`, ...). The gateway stores nothing, so there is no `previous_response_id`:
 * send the whole conversation each time. `timeout_ms` defaults to 120000 (at most 300000).
 */
export async function llm_respond(
  args: {
    input: string | unknown[];
    model?: string;
    instructions?: string;
    json_schema?: Record<string, unknown>;
    schema_name?: string;
    options?: Record<string, unknown>;
    timeout_ms?: number;
  },
  ctx: Ctx,
): Promise<{ output_text: string; response: ModelResponse }> {
  if (args.input === undefined || args.input === "") throw new Error("input is required");
  const body: Record<string, unknown> = { ...args.options, model: args.model || DEFAULT_MODEL, input: args.input };
  if (args.instructions) body.instructions = args.instructions;
  if (args.json_schema) {
    body.text = {
      format: { type: "json_schema", name: args.schema_name || "output", schema: args.json_schema, strict: true },
    };
  }
  body.store = false;
  body.stream = false;
  const timeout_ms = Math.min(Math.max(args.timeout_ms ?? DEFAULT_LLM_TIMEOUT_MS, 1000), MAX_LLM_TIMEOUT_MS);
  const response: ModelResponse = await call(ctx, `${LLM}/responses`, { method: "POST", body, timeout_ms });
  if (response.status === "failed" || response.error) {
    throw new Error(`Browser Use LLM request failed: ${errorDetail(response, JSON.stringify(response.error ?? response))}`);
  }
  return { output_text: outputText(response), response };
}

function outputText(response: ModelResponse): string {
  const parts: string[] = [];
  for (const item of (response.output ?? []) as any[]) {
    if (item?.type !== "message") continue;
    for (const content of item.content ?? []) {
      if (content?.type === "output_text" && typeof content.text === "string") parts.push(content.text);
    }
  }
  return parts.join("");
}

/**
 * Browser Use web search. `num_results` defaults to 8 (at most 20); `category` is `company`,
 * `people`, `news` or `publication`. Returns the result as sent; each search is charged.
 */
export async function web_search(
  args: { query: string; num_results?: number; category?: "company" | "people" | "news" | "publication" },
  ctx: Ctx,
): Promise<SearchResult> {
  if (!args.query || !args.query.trim()) throw new Error("query is required");
  const body: Record<string, unknown> = { query: args.query };
  if (args.num_results !== undefined) body.num_results = args.num_results;
  if (args.category) body.category = args.category;
  return call(ctx, SEARCH, { method: "POST", body, timeout_ms: 30000 });
}
