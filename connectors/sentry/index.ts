import type { Ctx, Response } from "../centaur";
import { query } from "../_lib/http";

// Results are Sentry's own REST payloads, passed through: issue, event and
// organization objects keep Sentry's field names, and events keep their
// `entries` (exception, stacktrace, breadcrumbs) untouched.

const DEFAULT_URL = "https://sentry.io";

/** A Sentry organization. */
export interface Organization {
  id: string;
  slug: string;
  name?: string;
  status?: { id: string; name: string };
  dateCreated?: string;
  isEarlyAdopter?: boolean;
  require2FA?: boolean;
  [field: string]: unknown;
}

/** A Sentry project. */
export interface Project {
  id: string;
  slug: string;
  name?: string;
  platform?: string | null;
  dateCreated?: string;
  isBookmarked?: boolean;
  isMember?: boolean;
  features?: string[];
  firstEvent?: string | null;
  [field: string]: unknown;
}

/** A Sentry issue (an error group). `count` and `userCount` are strings in Sentry's API. */
export interface Issue {
  id: string;
  /** Human-readable id such as `PROJECT-1A`; accepted wherever `issue_id` is. */
  shortId: string;
  title: string;
  culprit?: string;
  permalink?: string;
  logger?: string | null;
  level?: string;
  status?: string;
  substatus?: string;
  platform?: string;
  type?: string;
  metadata?: { title?: string; value?: string; type?: string; filename?: string; [field: string]: unknown };
  /** Number of events, as a string. */
  count?: string;
  userCount?: number;
  firstSeen?: string;
  lastSeen?: string;
  project?: { id: string; slug: string; name?: string; [field: string]: unknown };
  stats?: Record<string, unknown>;
  [field: string]: unknown;
}

/** One event (occurrence) of an issue. */
export interface Event {
  id: string;
  eventID?: string;
  groupID?: string;
  projectID?: string;
  title?: string;
  message?: string;
  platform?: string;
  dateCreated?: string;
  dateReceived?: string;
  /** Context payloads: `exception` (stacktrace frames), `breadcrumbs`, `request`, ... */
  entries?: { type: string; data?: Record<string, unknown>; [field: string]: unknown }[];
  tags?: { key: string; value: string }[];
  contexts?: Record<string, unknown>;
  user?: { id?: string; email?: string; username?: string; ip_address?: string; [field: string]: unknown };
  sdk?: { name?: string; version?: string };
  [field: string]: unknown;
}

/** One tag value's stats, as in `get_issue_tag_values`. */
export interface TagValueStats {
  count?: number;
  firstSeen?: string;
  lastSeen?: string;
  [field: string]: unknown;
}

/** `SENTRY_URL` is a base URL or host; the API lives under `/api/0`. */
function baseUrl(ctx: Ctx): string {
  let url = (ctx.env.SENTRY_URL || DEFAULT_URL).trim().replace(/\/+$/, "");
  if (url && !/^https?:\/\//.test(url)) url = `https://${url}`;
  return `${url}/api/0`;
}

async function sentry(
  ctx: Ctx,
  path: string,
  params: Record<string, string | number | boolean | null | undefined> = {},
): Promise<Response> {
  const token = ctx.env.SENTRY_AUTH_TOKEN;
  if (!token) {
    throw new Error("SENTRY_AUTH_TOKEN is not set; iron-proxy injects it for sentry.io hosts");
  }
  const res = await ctx.fetch(`${baseUrl(ctx)}${path}${query(params)}`, {
    headers: { accept: "application/json", authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new Error(`Sentry API error (${res.status}): ${(await res.text()).slice(0, 1000)}`);
  }
  return res;
}

async function sentryJson(
  ctx: Ctx,
  path: string,
  params: Record<string, string | number | boolean | null | undefined> = {},
): Promise<any> {
  return (await sentry(ctx, path, params)).json();
}

/**
 * The cursor of the `rel="next"` link in Sentry's `Link` header, or `null`.
 * Sentry pages 100 results at a time; pass the cursor back to get the next page.
 */
function nextCursor(res: Response): string | null {
  const link = res.headers.link;
  if (!link) return null;
  for (const part of link.split(",")) {
    if (!/rel="next"/.test(part)) continue;
    const cursor = /cursor="([^"]*)"/.exec(part);
    if (cursor) return cursor[1];
    // A next link without an explicit cursor attribute: read it off the URL.
    const url = /<([^>]+)>/.exec(part);
    const fromUrl = url && /[?&]cursor=([^&>]*)/.exec(url[1]);
    if (fromUrl) return decodeURIComponent(fromUrl[1]);
  }
  return null;
}

function checkLimit(limit: number | undefined, fallback: number): number {
  const value = limit === undefined ? fallback : limit;
  if (!Number.isInteger(value) || value < 1) throw new Error("limit must be a positive integer");
  return value;
}

const seg = (value: string): string => {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`missing required Sentry identifier: ${JSON.stringify(value)}`);
  }
  return encodeURIComponent(value.trim());
};

/**
 * Organizations the auth token can access: id, slug, name and status. Start here to find the
 * organization slug the other functions take.
 */
export async function list_organizations(args: {}, ctx: Ctx): Promise<{ organizations: Organization[] }> {
  return { organizations: await sentryJson(ctx, "/organizations/") };
}

/** Projects in an organization: id, slug, name, platform. Use the slug for `list_issues`. */
export async function list_projects(
  args: { organization_slug: string },
  ctx: Ctx,
): Promise<{ projects: Project[] }> {
  return { projects: await sentryJson(ctx, `/organizations/${seg(args.organization_slug)}/projects/`) };
}

/**
 * Search issues with Sentry's native query syntax (`is:unresolved level:error`, `environment:prod`,
 * `release:1.2.3`, `assigned:me`, `first-seen:-24h`), newest first by default. `sort` is `date`
 * (last seen), `new` (first seen), `freq` (events), `user` (users) or `priority`; `stats_period`
 * is the relative window (`24h`, `14d`, `90d`) that counts and sparklines cover. With
 * `project_slug` the search is scoped to that project, otherwise it spans the projects the token
 * can read. `limit` is per page (default 25, max 100); pass `next_cursor` back as `cursor` while
 * it is not null.
 */
export async function list_issues(
  args: {
    organization_slug: string;
    project_slug?: string;
    query?: string;
    sort?: "date" | "new" | "freq" | "user" | "priority";
    stats_period?: string;
    limit?: number;
    cursor?: string;
  },
  ctx: Ctx,
): Promise<{ issues: Issue[]; next_cursor: string | null }> {
  const params: Record<string, string | number> = {
    query: args.query ?? "is:unresolved",
    sort: args.sort ?? "date",
    statsPeriod: args.stats_period ?? "14d",
    limit: checkLimit(args.limit, 25),
  };
  if (args.cursor) params.cursor = args.cursor;
  const path = args.project_slug
    ? `/projects/${seg(args.organization_slug)}/${seg(args.project_slug)}/issues/`
    : `/organizations/${seg(args.organization_slug)}/issues/`;
  // Organization-wide search needs `project=-1` (all projects) in Sentry's API.
  if (!args.project_slug) params.project = -1;
  const res = await sentry(ctx, path, params);
  return { issues: await res.json(), next_cursor: nextCursor(res) };
}

/**
 * One issue by id: numeric id or short id such as `PROJECT-1A`. Adds `firstSeen`, `lastSeen`,
 * `count`, `userCount`, `stats` and the issue's `metadata` (error type and value) to the list
 * fields.
 */
export async function get_issue(
  args: { organization_slug: string; issue_id: string },
  ctx: Ctx,
): Promise<{ issue: Issue }> {
  const path = `/organizations/${seg(args.organization_slug)}/issues/${seg(args.issue_id)}/`;
  return { issue: await sentryJson(ctx, path) };
}

/**
 * Individual events (occurrences) of an issue, newest first. Without `full` each event is a
 * summary (`eventID`, `dateCreated`, `message`, `tags`); `full: true` includes each event's
 * `entries`. `limit` is per page (default 25, max 100); pass `next_cursor` back as `cursor`.
 */
export async function list_issue_events(
  args: { organization_slug: string; issue_id: string; full?: boolean; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ events: Event[]; next_cursor: string | null }> {
  const params: Record<string, string | number> = {
    full: args.full ? "true" : "false",
    per_page: checkLimit(args.limit, 25),
  };
  if (args.cursor) params.cursor = args.cursor;
  const path = `/organizations/${seg(args.organization_slug)}/issues/${seg(args.issue_id)}/events/`;
  const res = await sentry(ctx, path, params);
  return { events: await res.json(), next_cursor: nextCursor(res) };
}

/**
 * One event with its full payload: `entries` carries the `exception` type with the stacktrace
 * frames (innermost first, with in-app flag, filename, function, line numbers), breadcrumbs,
 * request and contexts. `event_id` is a specific event id, or `latest` (default) / `oldest`.
 */
export async function get_event(
  args: { organization_slug: string; issue_id: string; event_id?: string },
  ctx: Ctx,
): Promise<{ event: Event }> {
  const eventId = args.event_id ?? "latest";
  const path = `/organizations/${seg(args.organization_slug)}/issues/${seg(args.issue_id)}/events/${seg(eventId)}/`;
  return { event: await sentryJson(ctx, path) };
}

/**
 * Value distribution of one tag on an issue, keyed by tag value with each value's `count` and
 * first/last seen. Useful tags: `release`, `environment`, `browser`, `os`, `device`, `url`,
 * `transaction`, `user`, `level`.
 */
export async function get_issue_tag_values(
  args: { organization_slug: string; issue_id: string; tag_key: string },
  ctx: Ctx,
): Promise<{ values: Record<string, TagValueStats> }> {
  const path = `/organizations/${seg(args.organization_slug)}/issues/${seg(args.issue_id)}/tags/${seg(args.tag_key)}/`;
  return { values: await sentryJson(ctx, path) };
}
