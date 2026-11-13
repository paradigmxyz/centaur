import type { Ctx, Response, RequestInit } from "../centaur";
import { query, readJson } from "../_lib/http";

// The DocSend tool drives a remote Chromium (Browserbase or browser-use cloud)
// over CDP/WebSocket to log in, walk Spaces, fetch files and render PDFs. A Code
// Mode connector has only HTTP, so this connector covers the control plane that
// is plain HTTP: the resumable Browserbase session the Python client creates for
// each DocSend URL, and Browserbase's download records. The page-driving steps
// (download, login, resume, list, open-folder, fetch) are not available here.

const DEFAULT_BASE = "https://api.browserbase.com";
const DEFAULT_SESSION_TIMEOUT_SECONDS = 600;
const MAX_SESSION_TIMEOUT_SECONDS = 1800;
// Tool results are capped at 10 MiB and base64 inflates bytes by 4/3.
const MAX_DOWNLOAD_BYTES = 6 * 1024 * 1024;
const SESSION_ID_RE = /^[A-Za-z0-9_-]+$/;
const METADATA_LIMIT_BYTES = 512;

/** A Browserbase session, as Browserbase describes it. */
export interface BrowserbaseSession {
  /** Session ID, resumable while the session is PENDING or RUNNING. */
  id: string;
  /** CDP WebSocket URL; only returned when the session is created or running. */
  connectUrl: string | null;
  /** ISO timestamp after which the session stops. */
  expiresAt: string | null;
  /** `PENDING`, `RUNNING`, `ERROR`, `TIMED_OUT` or `COMPLETED`. */
  status: string;
  /** Carries `tool: "docsend"` and the encoded DocSend URL. */
  userMetadata: Record<string, unknown> | null;
  [field: string]: unknown;
}

/** One Browserbase download record for a session. */
export interface DownloadRecord {
  id: string;
  filename?: string;
  /** Bytes. */
  size?: number;
  [field: string]: unknown;
}

/** A downloaded file, base64. */
export interface DownloadContent {
  download_id: string;
  filename: string;
  size_bytes: number;
  content_base64: string;
}

function base(ctx: Ctx): string {
  return (ctx.env.BROWSERBASE_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

/** `https://host/path` on docsend.com, normalised (adds the scheme, drops a trailing slash). */
function normalizeDocsendUrl(url: string): string {
  let value = url.trim().replace(/\/+$/, "");
  if (!/^https?:\/\//i.test(value)) value = `https://${value}`;
  const match = /^(https?):\/\/([^/?#]*)([^#]*)/i.exec(value);
  if (!match) throw new Error("URL must be hosted on docsend.com");
  const host = (match[2].split("@").pop() ?? "").split(":")[0].toLowerCase();
  if (!(host === "docsend.com" || host.endsWith(".docsend.com"))) {
    throw new Error("URL must be hosted on docsend.com");
  }
  return value;
}

/** Encode a URL into Browserbase metadata (URL-safe base64, `b64` prefix). */
function encodeSessionUrl(url: string, ctx: Ctx): string {
  return `b64${ctx.base64.encode(url, { url: true })}`;
}

/** Decode a DocSend URL stored in Browserbase session metadata. */
function decodeSessionUrl(value: string, ctx: Ctx): string {
  if (!value.startsWith("b64")) throw new Error("Invalid DocSend URL in resumable session metadata");
  return normalizeDocsendUrl(ctx.base64.decodeText(value.slice(3)));
}

/** Browserbase's proxy config: the default proxy, or a country-pinned one. */
function proxyConfig(ctx: Ctx): boolean | { type: string; geolocation: { country: string } }[] {
  const country = (ctx.env.BROWSERBASE_PROXY_COUNTRY ?? "").trim();
  if (!country) return true;
  return [{ type: "browserbase", geolocation: { country: country.toUpperCase() } }];
}

function validateSessionId(sessionId: string): string {
  if (!SESSION_ID_RE.test(sessionId)) throw new Error("Invalid Browserbase session ID");
  return sessionId;
}

/** Browserbase errors carry `{ message }` or `{ error }`; surface that. */
async function browserbaseError(res: Response): Promise<Error> {
  const text = await res.text();
  let detail = "";
  try {
    const body = JSON.parse(text);
    if (body && typeof body === "object") detail = String(body.message ?? body.error ?? "");
  } catch {
    detail = text;
  }
  const suffix = detail ? `: ${detail.slice(0, 1000)}` : "";
  return new Error(`Browserbase API error ${res.status}${suffix}`);
}

async function request(ctx: Ctx, path: string, init: RequestInit = {}): Promise<Response> {
  const headers: Record<string, string | undefined> = { accept: "application/json", ...init.headers };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.BROWSERBASE_API_KEY) headers["x-bb-api-key"] = ctx.env.BROWSERBASE_API_KEY;
  const res = await ctx.fetch(base(ctx) + path, { ...init, headers });
  if (!res.ok) throw await browserbaseError(res);
  return res;
}

/** Read a session response into the flat shape this connector returns. */
function sessionView(body: Record<string, any>, ctx: Ctx): { session: BrowserbaseSession; docsend_url: string | null } {
  const id = body.id;
  if (typeof id !== "string" || !id) throw new Error("Browserbase session response did not include a session ID");
  const metadata = (body.userMetadata ?? null) as Record<string, unknown> | null;
  const encoded = metadata?.docsend_url;
  return {
    session: {
      id,
      connectUrl: typeof body.connectUrl === "string" ? body.connectUrl : null,
      expiresAt: body.expiresAt == null ? null : String(body.expiresAt),
      status: String(body.status ?? ""),
      userMetadata: metadata,
    },
    docsend_url: typeof encoded === "string" ? decodeSessionUrl(encoded, ctx) : null,
  };
}

/**
 * Create a resumable Browserbase session for a DocSend URL, the way the Python
 * client starts every download or Space login. `url` must be on docsend.com;
 * `timeout_seconds` is the session lifetime (60–1800, default 600) and the
 * session survives disconnects (`keepAlive`). Returns the session (id, CDP
 * `connectUrl`, `expiresAt`, `status`) and the normalised DocSend URL. Pass the
 * id to `get_session`/`release_session`. Driving the browser itself needs CDP,
 * which a cell cannot do.
 */
export async function create_session(
  args: { url: string; timeout_seconds?: number },
  ctx: Ctx,
): Promise<{ session: BrowserbaseSession; docsend_url: string }> {
  const url = normalizeDocsendUrl(args.url);
  const timeoutSeconds = args.timeout_seconds ?? DEFAULT_SESSION_TIMEOUT_SECONDS;
  if (!(timeoutSeconds >= 60 && timeoutSeconds <= MAX_SESSION_TIMEOUT_SECONDS)) {
    throw new Error(`timeout_seconds must be between 60 and ${MAX_SESSION_TIMEOUT_SECONDS}`);
  }
  const userMetadata = { tool: "docsend", docsend_url: encodeSessionUrl(url, ctx) };
  if (JSON.stringify(userMetadata).length >= METADATA_LIMIT_BYTES) {
    throw new Error("DocSend URL is too long for a resumable Browserbase session");
  }
  const res = await request(ctx, "/v1/sessions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      proxies: proxyConfig(ctx),
      keepAlive: true,
      timeout: timeoutSeconds,
      userMetadata,
    }),
  });
  const parsed = sessionView(await readJson(res, "Browserbase"), ctx);
  return { session: parsed.session, docsend_url: url };
}

/**
 * Fetch one Browserbase session: `status` (`PENDING`/`RUNNING` are still usable,
 * `COMPLETED`/`TIMED_OUT`/`ERROR` are not), `expiresAt`, `connectUrl` and the
 * DocSend URL recorded when it was created. Fails when the session was not
 * created by this tool.
 */
export async function get_session(
  args: { session_id: string },
  ctx: Ctx,
): Promise<{ session: BrowserbaseSession; docsend_url: string }> {
  const sessionId = validateSessionId(args.session_id);
  const res = await request(ctx, `/v1/sessions/${encodeURIComponent(sessionId)}`);
  const parsed = sessionView(await readJson(res, "Browserbase"), ctx);
  if (parsed.session.userMetadata?.tool !== "docsend" || !parsed.docsend_url) {
    throw new Error("The Browserbase session does not belong to the DocSend tool.");
  }
  return { session: parsed.session, docsend_url: parsed.docsend_url };
}

/**
 * Release a Browserbase session early (`REQUEST_RELEASE`) so it stops billing
 * before its timeout. Safe to call on an already finished session; it does not
 * delete the download records or replays.
 */
export async function release_session(
  args: { session_id: string },
  ctx: Ctx,
): Promise<{ session_id: string; released: boolean }> {
  const sessionId = validateSessionId(args.session_id);
  await request(ctx, `/v1/sessions/${encodeURIComponent(sessionId)}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ status: "REQUEST_RELEASE" }),
  });
  return { session_id: sessionId, released: true };
}

/**
 * The files a session's browser downloaded, from Browserbase's download records
 * (id, filename, size, and the URL it came from). `limit` defaults to 100. Use
 * `download_content` to fetch a record's bytes.
 */
export async function list_downloads(
  args: { session_id: string; limit?: number },
  ctx: Ctx,
): Promise<{ downloads: DownloadRecord[] }> {
  const sessionId = validateSessionId(args.session_id);
  const res = await request(
    ctx,
    `/v1/downloads${query({ sessionId, limit: Math.min(args.limit ?? 100, 100) })}`,
  );
  const body = await readJson(res, "Browserbase");
  if (!Array.isArray(body.downloads)) throw new Error("Browserbase returned an invalid downloads list");
  return { downloads: body.downloads as DownloadRecord[] };
}

/**
 * One download record's bytes, base64-encoded, with `filename` and `size_bytes`.
 * Capped at 6 MiB (base64 in a JSON result is capped at 10 MiB); larger files
 * fail. Write it with the caller's file tools.
 */
export async function download_content(
  args: { download_id: string },
  ctx: Ctx,
): Promise<DownloadContent> {
  const downloadId = args.download_id;
  if (!SESSION_ID_RE.test(downloadId)) throw new Error("Invalid Browserbase download ID");
  const res = await request(ctx, `/v1/downloads/${encodeURIComponent(downloadId)}`, {
    headers: { accept: "application/octet-stream" },
  });
  const disclosure = res.headers["content-disposition"] ?? "";
  const named = /filename\*?=(?:UTF-8'')?"?([^";]+)"?/i.exec(disclosure);
  let filename = downloadId;
  if (named) {
    try {
      filename = decodeURIComponent(named[1].trim());
    } catch {
      filename = named[1].trim();
    }
  }
  const bytes = await res.bytes();
  if (bytes.length > MAX_DOWNLOAD_BYTES) {
    throw new Error(`Downloaded file exceeds the ${MAX_DOWNLOAD_BYTES / (1024 * 1024)} MiB limit`);
  }
  return {
    download_id: downloadId,
    filename,
    size_bytes: bytes.length,
    content_base64: ctx.base64.encode(bytes),
  };
}
