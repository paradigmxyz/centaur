import type { Ctx } from "../centaur";
import type { Params } from "../_lib/http";
import { encode } from "../_lib/http";

// Luma's public API (public-api.luma.com), authenticated with a calendar API key.
// Endpoints answer JSON envelopes the connector passes through unchanged
// (`{ entries, has_more, next_cursor }` for lists); `request` reaches anything
// not wrapped here (contacts, tags, tickets, webhooks).

const DEFAULT_BASE = "https://public-api.luma.com";

/** The user an API key belongs to. */
export interface LumaUser {
  api_id?: string;
  name?: string;
  email?: string;
  avatar_url?: string;
  [field: string]: unknown;
}

/** A calendar. */
export interface LumaCalendar {
  api_id?: string;
  name?: string;
  description?: string;
  avatar_url?: string;
  timezone?: string;
  [field: string]: unknown;
}

/** A calendar event. */
export interface LumaEvent {
  api_id?: string;
  name?: string;
  description?: string;
  /** ISO 8601 UTC start. */
  start_at?: string;
  /** ISO 8601 UTC end. */
  end_at?: string;
  timezone?: string;
  url?: string;
  cover_url?: string;
  [field: string]: unknown;
}

/** A guest of an event. */
export interface LumaGuest {
  api_id?: string;
  user_email?: string;
  user_name?: string;
  approval_status?: string;
  [field: string]: unknown;
}

/** A paginated list; pass `next_cursor` back as `cursor` for the next page. */
export interface EventList {
  entries?: LumaEvent[];
  has_more?: boolean;
  next_cursor?: string | null;
  [field: string]: unknown;
}

/** A paginated guest list; pass `next_cursor` back as `cursor` for the next page. */
export interface GuestList {
  entries?: LumaGuest[];
  has_more?: boolean;
  next_cursor?: string | null;
  [field: string]: unknown;
}

function base(ctx: Ctx): string {
  return (ctx.env.LUMA_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx, calendar_id: string | undefined): Record<string, string> {
  const value: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.LUMA_API_KEY) value["x-luma-api-key"] = ctx.env.LUMA_API_KEY;
  if (calendar_id) value["x-luma-calendar-id"] = calendar_id;
  return value;
}

/** Arrays become repeated parameters (`platforms=a&platforms=b`), as Luma requires. */
function search(params: Record<string, unknown> | undefined): string {
  if (!params) return "";
  const scalars: Params = {};
  const parts: string[] = [];
  for (const key of Object.keys(params)) {
    const value = params[key];
    if (value === undefined || value === null || value === "") continue;
    if (Array.isArray(value)) {
      // Luma wants repeated parameters (`platforms=a&platforms=b`), not one comma list.
      for (const item of value) {
        if (item === undefined || item === null || item === "") continue;
        parts.push(`${encodeURIComponent(key)}=${encodeURIComponent(String(item))}`);
      }
    } else if (typeof value === "object") {
      parts.push(`${encodeURIComponent(key)}=${encodeURIComponent(JSON.stringify(value))}`);
    } else {
      scalars[key] = value as string | number | boolean;
    }
  }
  const encoded = encode(scalars);
  if (encoded) parts.unshift(encoded);
  return parts.length ? `?${parts.join("&")}` : "";
}

/** Only Luma API paths, never another host or an absolute URL. */
function endpoint(path: string): string {
  let value = (path || "").trim();
  if (!value) throw new Error("path is required");
  if (value.includes("://") || value.startsWith("//")) {
    throw new Error("path must be a Luma API path, not a URL");
  }
  if (!value.startsWith("/")) value = `/${value}`;
  if (!value.startsWith("/v1/") && !value.startsWith("/v2/")) {
    throw new Error("path must start with /v1/ or /v2/");
  }
  return value;
}

/** Luma errors carry `message`, `error` or `detail`; fall back to the raw body. */
function errorMessage(payload: any, text: string): string {
  if (payload && typeof payload === "object") {
    for (const key of ["message", "error", "detail"]) {
      const value = payload[key];
      if (typeof value === "string" && value) return value;
      if (value && typeof value === "object" && typeof value.message === "string" && value.message) {
        return value.message;
      }
    }
  }
  return text.trim();
}

async function request(
  ctx: Ctx,
  method: string,
  path: string,
  options: { params?: Record<string, unknown>; body?: unknown; calendar_id?: string } = {},
): Promise<any> {
  const requestHeaders = headers(ctx, options.calendar_id);
  if (options.body !== undefined) requestHeaders["content-type"] = "application/json";
  let res = await ctx.fetch(`${base(ctx)}${endpoint(path)}${search(options.params)}`, {
    method: method.toUpperCase(),
    headers: requestHeaders,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  // Rate limits are retried twice by default; a call may take at most ~60 s.
  for (let attempt = 0; res.status === 429 && attempt < 2; attempt += 1) {
    const retry = Number(res.headers["retry-after"] || "1");
    await ctx.sleep(Math.min(Number.isFinite(retry) ? retry : 1, 5) * 1000);
    res = await ctx.fetch(`${base(ctx)}${endpoint(path)}${search(options.params)}`, {
      method: method.toUpperCase(),
      headers: requestHeaders,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
  }
  const text = await res.text();
  if (res.status >= 400) {
    let payload: any;
    try {
      payload = text ? JSON.parse(text) : undefined;
    } catch {
      payload = undefined;
    }
    throw new Error(`Luma API error (${res.status}): ${errorMessage(payload, text)}`);
  }
  if (res.status === 204 || !text) return {};
  const payload = JSON.parse(text);
  if (!payload || typeof payload !== "object") {
    throw new Error("Luma API returned an unexpected JSON response");
  }
  return payload;
}

/** The user the API key belongs to, as `{ user }`. */
export async function get_self(args: {}, ctx: Ctx): Promise<{ user: LumaUser }> {
  return request(ctx, "GET", "/v1/users/get-self");
}

/**
 * The calendar the key belongs to, or one organization calendar when `calendar_id` is given
 * (sent as the `x-luma-calendar-id` header, which requires an organization API key).
 */
export async function get_calendar(
  args: { calendar_id?: string },
  ctx: Ctx,
): Promise<{ calendar: LumaCalendar }> {
  return request(ctx, "GET", "/v1/calendars/get", { calendar_id: args.calendar_id });
}

/**
 * List calendar events as `{ entries, has_more, next_cursor }`. `after`/`before` are ISO 8601
 * bounds on the start time; `platforms` (`luma`, `external`), `access` (`public`, `private`) and
 * `status` (`approved`, ...) filter; `sort_direction` (`asc` or `desc`) sorts by start time.
 * Page with `cursor` = the previous `next_cursor` and `limit` (default 20ish, max 100).
 * `calendar_id` selects an organization calendar.
 */
export async function list_events(
  args: {
    before?: string;
    after?: string;
    cursor?: string;
    limit?: number;
    platforms?: string[];
    access?: string[];
    status?: string;
    sort_direction?: string;
    calendar_id?: string;
  },
  ctx: Ctx,
): Promise<EventList> {
  const params: Record<string, unknown> = {
    before: args.before,
    after: args.after,
    pagination_cursor: args.cursor,
    pagination_limit: args.limit,
    platforms: args.platforms,
    access: args.access,
    status: args.status,
    sort_column: args.sort_direction ? "start_at" : undefined,
    sort_direction: args.sort_direction,
  };
  return request(ctx, "GET", "/v1/calendars/events/list", {
    params,
    calendar_id: args.calendar_id,
  });
}

/** One event by its `evt-…` ID, as `{ event }`. */
export async function get_event(
  args: { event_id: string },
  ctx: Ctx,
): Promise<{ event: LumaEvent }> {
  if (!args.event_id || !args.event_id.trim()) throw new Error("event_id is required");
  return request(ctx, "GET", "/v1/events/get", { params: { event_id: args.event_id.trim() } });
}

/**
 * Create an event from a Luma `EventCreateRequest` body: `event` carries the vendor fields
 * (`name`, `start_at`, `end_at`, `timezone`, `description_mirror`, ...) and is sent as the JSON
 * body unchanged. `calendar_id` creates it on an organization calendar. Returns `{ event }`.
 */
export async function create_event(
  args: { event: Record<string, unknown>; calendar_id?: string },
  ctx: Ctx,
): Promise<{ event: LumaEvent }> {
  if (!args.event || typeof args.event !== "object") throw new Error("event is required (JSON object)");
  return request(ctx, "POST", "/v1/events/create", {
    body: args.event,
    calendar_id: args.calendar_id,
  });
}

/**
 * Update an event. `event` must include `event_id` plus the fields to change (Luma replaces the
 * ones you send, so send the full field set you want to keep) and is passed through unchanged.
 * `calendar_id` targets an organization calendar. Returns `{ event }`.
 */
export async function update_event(
  args: { event: Record<string, unknown>; calendar_id?: string },
  ctx: Ctx,
): Promise<{ event: LumaEvent }> {
  if (!args.event || typeof args.event !== "object") throw new Error("event is required (JSON object)");
  if (!args.event.event_id) throw new Error("event.event_id is required");
  return request(ctx, "POST", "/v1/events/update", {
    body: args.event,
    calendar_id: args.calendar_id,
  });
}

/**
 * List an event's guests as `{ entries, has_more, next_cursor }`. `approval_status`
 * (`approved`, `pending_approval`, `declined`, ...) filters; `sort_column` (`name`,
 * `registered_at`, ...) and `sort_direction` order; page with `cursor` = `next_cursor` and
 * `limit`.
 */
export async function list_guests(
  args: {
    event_id: string;
    approval_status?: string;
    cursor?: string;
    limit?: number;
    sort_column?: string;
    sort_direction?: string;
  },
  ctx: Ctx,
): Promise<GuestList> {
  if (!args.event_id || !args.event_id.trim()) throw new Error("event_id is required");
  return request(ctx, "GET", "/v1/events/guests/list", {
    params: {
      event_id: args.event_id.trim(),
      approval_status: args.approval_status,
      pagination_cursor: args.cursor,
      pagination_limit: args.limit,
      sort_column: args.sort_column,
      sort_direction: args.sort_direction,
    },
  });
}

/**
 * Call any Luma API endpoint and get its JSON back unchanged, under `response`: contacts, tags,
 * tickets, webhooks and anything else not wrapped above. `path` must start with `/v1/` or `/v2/`
 * and stay on Luma's API host. `params` become query parameters (array values are repeated,
 * objects are JSON-encoded) and `body` is the JSON body. `calendar_id` adds the
 * `x-luma-calendar-id` header for organization keys. Use GET for reads.
 */
export async function request_endpoint(
  args: {
    method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
    path: string;
    params?: Record<string, unknown>;
    body?: Record<string, unknown>;
    calendar_id?: string;
  },
  ctx: Ctx,
): Promise<{ response: Record<string, any> }> {
  const payload = await request(ctx, args.method || "GET", args.path, {
    params: args.params,
    body: args.body,
    calendar_id: args.calendar_id,
  });
  return { response: payload };
}
