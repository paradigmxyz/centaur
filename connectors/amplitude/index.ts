import type { Ctx } from "../centaur";
import { readJson } from "../_lib/http";

// Results are Amplitude's own Dashboard REST / Taxonomy API JSON, passed
// through: chart endpoints keep their `data.xValues` / `data.series`
// envelope, taxonomy endpoints are unwrapped from their top-level array.
// Field names are Amplitude's (https://amplitude.com/docs/apis).

const US_HOST = "amplitude.com";
const EU_HOST = "analytics.eu.amplitude.com";

/** Chart payload shared by segmentation, funnel, retention and realtime. */
export interface ChartSeries {
  /** Bucket labels aligned with `series` (date strings for daily/weekly/monthly intervals). */
  xValues?: (string | number)[];
  /** One array per series, aligned with `xValues`. */
  series?: (number | null)[][];
  seriesLabels?: string[];
  [field: string]: unknown;
}

/** Amplitude wraps every chart response in `data`. */
export interface ChartResponse {
  data?: ChartSeries;
  [field: string]: unknown;
}

/** One entry of `GET /api/2/events/list`. */
export interface EventType {
  name: string;
  display?: string;
  non_active?: boolean;
  [field: string]: unknown;
}

/** One event of a user's stream (`user_activity`). */
export interface AmplitudeEvent {
  event_type: string;
  /** ISO-8601 UTC. */
  event_time?: string;
  user_id?: string | null;
  device_id?: string | null;
  session_id?: number | null;
  event_properties?: Record<string, unknown>;
  user_properties?: Record<string, unknown>;
  [field: string]: unknown;
}

/** `GET /api/2/useractivity`: the user's profile plus their most recent events. */
export interface UserActivityResponse {
  userData?: {
    user_id?: string;
    canonical_amplitude_id?: number;
    device_ids?: string[];
    [field: string]: unknown;
  };
  events?: AmplitudeEvent[];
  pageInfo?: { offset?: number; limit?: number; total?: number; [field: string]: unknown };
  [field: string]: unknown;
}

/** One `GET /api/2/usersearch` match. */
export interface UserMatch {
  amplitude_id?: number;
  canonical_amplitude_id?: number;
  user_id?: string | null;
  device_id?: string | null;
  last_seen?: number;
  [field: string]: unknown;
}

/** A chart annotation. */
export interface Annotation {
  id?: number;
  /** YYYY-MM-DD. */
  start?: string;
  end?: string;
  label?: string;
  category?: string;
  [field: string]: unknown;
}

/** A tracking-plan event definition (`taxonomy_events`). */
export interface TaxonomyEvent {
  event_type: string;
  category?: string;
  tags?: string[];
  description?: string;
  [field: string]: unknown;
}

/** A tracking-plan event property (`taxonomy_event_properties`). */
export interface TaxonomyEventProperty {
  event_type?: string;
  event_property: string;
  type?: string;
  [field: string]: unknown;
}

/** A tracking-plan user property (`taxonomy_user_properties`). */
export interface TaxonomyUserProperty {
  user_property: string;
  type?: string;
  [field: string]: unknown;
}

/** YYYY-MM-DD, YYYY/MM/DD or YYYYMMDD to Amplitude's YYYYMMDD. */
function fmtDate(value: string): string {
  return value.replace(/[-/]/g, "").trim();
}

function baseUrl(ctx: Ctx): string {
  const region = (ctx.env.AMPLITUDE_REGION || "us").trim().toLowerCase();
  return `https://${region === "eu" ? EU_HOST : US_HOST}`;
}

/**
 * Amplitude reads HTTP Basic `{api_key}:{secret_key}`. The placeholders are swapped in the
 * base64 header by the proxy; locally the real keys come from the environment.
 */
function authorization(ctx: Ctx): string {
  const apiKey = ctx.env.AMPLITUDE_API_KEY || "AMPLITUDE_API_KEY";
  const secretKey = ctx.env.AMPLITUDE_SECRET_KEY || "AMPLITUDE_SECRET_KEY";
  return `Basic ${ctx.base64.encode(`${apiKey}:${secretKey}`)}`;
}

/** `e` (and funnels' repeated `e`) take compact JSON event specs. */
function eventSpec(eventType: string): string {
  return JSON.stringify({ event_type: eventType });
}

type Pair = [string, string];

function search(pairs: Pair[]): string {
  const encoded = pairs
    .filter(([, value]) => value !== "")
    .map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`)
    .join("&");
  return encoded ? `?${encoded}` : "";
}

async function request(ctx: Ctx, path: string, pairs: Pair[] = []): Promise<any> {
  const res = await ctx.fetch(`${baseUrl(ctx)}${path}${search(pairs)}`, {
    headers: { accept: "application/json", authorization: authorization(ctx) },
  });
  return readJson(res, "Amplitude");
}

/**
 * Counts or uniques of one event over time, optionally broken down by one event or user property.
 * `start`/`end` are YYYY-MM-DD, YYYY/MM/DD or YYYYMMDD (inclusive, Amplitude's project timezone).
 * `metric` is totals, uniques, pct_dau, average, histogram, sums or value_avg; `interval` is 1
 * (daily), 7 (weekly) or 30 (monthly); use "_all" as `event` for any active event. Returns
 * `data.xValues` with one `data.series` array per breakdown value (`data.seriesLabels`),
 * `segment` is an array of Amplitude segment definitions. `limit` caps breakdown groups.
 */
export async function segmentation(
  args: {
    event: string;
    start: string;
    end: string;
    metric?: string;
    interval?: number;
    group_by?: string;
    group_by_type?: "event" | "user";
    segment?: Record<string, unknown>[];
    limit?: number;
  },
  ctx: Ctx,
): Promise<ChartResponse> {
  const pairs: Pair[] = [
    ["e", eventSpec(args.event)],
    ["start", fmtDate(args.start)],
    ["end", fmtDate(args.end)],
    ["m", args.metric ?? "totals"],
    ["i", String(args.interval ?? 1)],
    ["limit", String(args.limit ?? 100)],
  ];
  if (args.group_by) {
    const groupBy = [{ type: args.group_by_type ?? "event", value: args.group_by }];
    pairs[0] = ["e", JSON.stringify({ event_type: args.event, group_by: groupBy })];
  }
  if (args.segment?.length) pairs.push(["s", JSON.stringify(args.segment)]);
  return request(ctx, "/api/2/events/segmentation", pairs);
}

/**
 * Conversion across an ordered sequence of events. `events` are event types in step order, `start`
 * and `end` as in `segmentation`, `mode` is ordered, unordered or sequential.
 * `conversion_window_days` is sent as Amplitude's `cs` (milliseconds).
 */
export async function funnel(
  args: {
    events: string[];
    start: string;
    end: string;
    mode?: "ordered" | "unordered" | "sequential";
    conversion_window_days?: number;
  },
  ctx: Ctx,
): Promise<ChartResponse> {
  if (!args.events.length) throw new Error("funnel needs at least one event type");
  const pairs: Pair[] = args.events.map((event) => ["e", eventSpec(event)]);
  pairs.push(
    ["start", fmtDate(args.start)],
    ["end", fmtDate(args.end)],
    ["mode", args.mode ?? "ordered"],
  );
  if (args.conversion_window_days !== undefined) {
    pairs.push(["cs", String(args.conversion_window_days * 86_400_000)]);
  }
  return request(ctx, "/api/2/funnels", pairs);
}

/**
 * Retention of users who did `start_event` and later did `return_event` ("_all" for any event).
 * `retention_mode` is n-day, rolling or bracket; `interval` is 1 (daily), 7 (weekly) or 30
 * (monthly). `start`/`end` select the cohort window, as in `segmentation`.
 */
export async function retention(
  args: {
    start_event: string;
    return_event?: string;
    start: string;
    end: string;
    retention_mode?: "n-day" | "rolling" | "bracket";
    interval?: number;
  },
  ctx: Ctx,
): Promise<ChartResponse> {
  return request(ctx, "/api/2/retention", [
    ["se", eventSpec(args.start_event)],
    ["re", eventSpec(args.return_event ?? "_all")],
    ["start", fmtDate(args.start)],
    ["end", fmtDate(args.end)],
    ["rm", args.retention_mode ?? "n-day"],
    ["i", String(args.interval ?? 1)],
  ]);
}

/** Every event type defined in the project, with display name and whether it is non-active. */
export async function events_list(args: {}, ctx: Ctx): Promise<{ events: EventType[] }> {
  const body = await request(ctx, "/api/2/events/list");
  return { events: Array.isArray(body) ? body : (body.data ?? []) };
}

/**
 * One user's event stream by Amplitude ID (`user`). `limit` is at most 1000, `offset` paginates,
 * `direction` is latest or earliest. Returns the user profile (`userData`), their events newest
 * first, and `pageInfo`.
 */
export async function user_activity(
  args: { user: string; limit?: number; offset?: number; direction?: "latest" | "earliest" },
  ctx: Ctx,
): Promise<UserActivityResponse> {
  return request(ctx, "/api/2/useractivity", [
    ["user", args.user],
    ["limit", String(args.limit ?? 100)],
    ["offset", String(args.offset ?? 0)],
    ["direction", args.direction ?? "latest"],
  ]);
}

/**
 * Resolve an Amplitude ID, user ID, device ID or a prefix of one to matching users, with their
 * canonical Amplitude ID and last-seen time.
 */
export async function user_search(
  args: { user: string },
  ctx: Ctx,
): Promise<{ matches: UserMatch[] }> {
  const body = await request(ctx, "/api/2/usersearch", [["user", args.user]]);
  return { matches: body.matches ?? body.data ?? [] };
}

/**
 * Real-time active-user counts for the last 24 hours. `interval` is the bucket size in seconds
 * (for example 300 for five-minute buckets); omit for Amplitude's default buckets.
 */
export async function realtime(
  args: { interval?: number },
  ctx: Ctx,
): Promise<ChartResponse> {
  const pairs: Pair[] = args.interval ? [["i", String(-args.interval * 1000)]] : [];
  return request(ctx, "/api/2/realtime", pairs);
}

/** Chart annotations for the project (deploy markers and other dated notes). */
export async function annotations(args: {}, ctx: Ctx): Promise<{ annotations: Annotation[] }> {
  const body = await request(ctx, "/api/2/annotations");
  return { annotations: Array.isArray(body) ? body : (body.data ?? []) };
}

/** Event types in the project's tracking plan (Taxonomy API), with category, tags, description. */
export async function taxonomy_events(args: {}, ctx: Ctx): Promise<{ events: TaxonomyEvent[] }> {
  const body = await request(ctx, "/api/2/taxonomy/event");
  return { events: Array.isArray(body) ? body : (body.data ?? []) };
}

/** Event properties the tracking plan defines for one event type, with their types. */
export async function taxonomy_event_properties(
  args: { event_type: string },
  ctx: Ctx,
): Promise<{ event_properties: TaxonomyEventProperty[] }> {
  const body = await request(ctx, "/api/2/taxonomy/event-property", [
    ["event_type", args.event_type],
  ]);
  return { event_properties: Array.isArray(body) ? body : (body.data ?? []) };
}

/** User properties in the project's tracking plan (Taxonomy API), with their types. */
export async function taxonomy_user_properties(
  args: {},
  ctx: Ctx,
): Promise<{ user_properties: TaxonomyUserProperty[] }> {
  const body = await request(ctx, "/api/2/taxonomy/user-property");
  return { user_properties: Array.isArray(body) ? body : (body.data ?? []) };
}
