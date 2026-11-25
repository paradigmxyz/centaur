import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Pylon answers `{ data, pagination? }` envelopes; results here are Pylon's own
// objects with the envelope unwrapped, so field names are Pylon's. Cursor-paged
// endpoints return `pagination.next_cursor`/`has_next_page`: pass the cursor
// back while there are more pages.

const DEFAULT_BASE = "https://api.usepylon.com";
const MAX_PAGE = 1000;

export interface Pagination {
  cursor?: string | null;
  has_next_page?: boolean;
  [field: string]: unknown;
}

/** A user reference as Pylon embeds it (requester, assignee, owner). */
export interface UserRef {
  id?: string;
  name?: string | null;
  email?: string | null;
  [field: string]: unknown;
}

export interface AccountRef {
  id?: string;
  name?: string | null;
  [field: string]: unknown;
}

export interface Team {
  id: string;
  name?: string;
  [field: string]: unknown;
}

/** The organization that owns the API token, from `GET /me`. */
export interface Organization {
  id?: string;
  name?: string;
  [field: string]: unknown;
}

export interface Issue {
  id: string;
  /** Human-facing issue number. */
  number?: number | string;
  title?: string;
  /** HTML body as Pylon stores it. */
  body_html?: string | null;
  /** new, waiting_on_you, waiting_on_customer, on_hold or closed. */
  state?: string;
  /** urgent, high, medium or low. */
  priority?: string | null;
  tags?: string[];
  requester?: UserRef;
  assignee?: UserRef | null;
  team?: Team | null;
  account?: AccountRef | null;
  /** ISO 8601 timestamps. */
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

export interface Account {
  id: string;
  name?: string;
  /** Domains without a scheme, for example ["acme.com"]. */
  domains?: string[];
  primary_domain?: string | null;
  tags?: string[];
  owner?: UserRef | null;
  external_id?: string | null;
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

export interface Contact {
  id: string;
  name?: string;
  email?: string | null;
  account?: AccountRef | null;
  [field: string]: unknown;
}

export interface User {
  id: string;
  name?: string;
  email?: string;
  role?: { name?: string; [field: string]: unknown } | null;
  [field: string]: unknown;
}

export interface Tag {
  id?: string;
  /** The tag text. */
  value?: string;
  /** account, issue or contact. */
  object_type?: string;
  hex_color?: string;
  [field: string]: unknown;
}

/**
 * A Pylon search filter: `{ field, operator, value }` — for example
 * `{ field: "state", operator: "equals", value: "new" }`, `{ field: "title", operator: "string_contains",
 * value: "outage" }`, `{ field: "assignee_id", operator: "is_unset" }`. Compound filters nest under
 * `{ operator: "and" | "or", value: [filter, ...] }`.
 */
export interface Filter {
  field?: string;
  operator: string;
  value?: unknown;
  [field: string]: unknown;
}

interface RequestOptions {
  query?: Params;
  json?: unknown;
}

/** The vendor's message from an error body: `{ error: { message } }`, `{ errors: [...] }`, `{ code }`. */
function errorDetail(text: string): string {
  try {
    const body = JSON.parse(text) as {
      error?: { message?: unknown };
      errors?: unknown;
      message?: unknown;
      code?: unknown;
    };
    const nested = body.error?.message;
    if (typeof nested === "string" && nested) return nested;
    if (Array.isArray(body.errors)) {
      const messages = body.errors.map((item) =>
        typeof item === "string" ? item : typeof item?.message === "string" ? String(item.message) : "",
      );
      const joined = messages.filter(Boolean).join("; ");
      if (joined) return joined;
    }
    if (typeof body.message === "string" && body.message) return body.message;
    if (typeof body.code === "string" && body.code) return body.code;
  } catch {
    // Not JSON; fall through to the raw text.
  }
  return text.slice(0, 1000);
}

async function request(
  ctx: Ctx,
  method: string,
  path: string,
  options: RequestOptions = {},
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the token is injected on the wire; locally it comes from the env.
  const key = ctx.env.PYLON_API_KEY;
  if (key) headers.authorization = `Bearer ${key}`;
  let body: string | undefined;
  if (options.json !== undefined) {
    headers["content-type"] = "application/json";
    body = JSON.stringify(options.json);
  }
  const url = (ctx.env.PYLON_API_URL || DEFAULT_BASE).replace(/\/+$/, "") + path + query(options.query ?? {});
  const res = await ctx.fetch(url, { method, headers, body });
  const text = await res.text();
  if (!res.ok) throw new Error(`Pylon ${method} ${path} returned HTTP ${res.status}: ${errorDetail(text)}`);
  return text ? JSON.parse(text) : {};
}

const seg = (value: string): string => encodeURIComponent(value.trim());

/** An ISO 8601 / RFC3339 timestamp for a date (`YYYY-MM-DD`, UTC midnight) or time string. */
function timestamp(value: string, label: string): string {
  const raw = value.trim();
  const millis = Date.parse(/^\d{4}-\d{2}-\d{2}$/.test(raw) ? `${raw}T00:00:00Z` : raw);
  if (Number.isNaN(millis)) throw new Error(`${label} ${JSON.stringify(value)} is not YYYY-MM-DD or an ISO time`);
  return new Date(millis).toISOString();
}

function pageSize(limit: number | undefined, fallback: number): number {
  return Math.min(Math.max(Math.trunc(limit ?? fallback), 1), MAX_PAGE);
}

/** The organization that owns the API token: ID and name. Useful as an auth/health check. */
export async function me(args: {}, ctx: Ctx): Promise<{ organization: Organization }> {
  return { organization: ((await request(ctx, "GET", "/me")).data ?? {}) as Organization };
}

/**
 * Issues created in a time window, in the order Pylon returns them. Defaults to the last `days` days
 * (default 7) when `start_time`/`end_time` are not given; both accept `YYYY-MM-DD` or an ISO time and are
 * sent as RFC3339. Page with `pagination.cursor`, or use `search_issues` to filter.
 */
export async function list_issues(
  args: { start_time?: string; end_time?: string; days?: number; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ issues: Issue[]; pagination?: Pagination }> {
  const end = args.end_time ? timestamp(args.end_time, "end_time") : new Date().toISOString();
  const days = Math.max(Math.trunc(args.days ?? 7), 1);
  const start = args.start_time
    ? timestamp(args.start_time, "start_time")
    : new Date(Date.parse(end) - days * 24 * 60 * 60 * 1000).toISOString();
  const body = await request(ctx, "GET", "/issues", {
    query: { start_time: start, end_time: end, limit: pageSize(args.limit, 100), cursor: args.cursor },
  });
  return { issues: Array.isArray(body.data) ? (body.data as Issue[]) : [], pagination: body.pagination };
}

/** One issue by ID or number: title, HTML body, state, priority, tags, requester, assignee, team, account, timestamps. */
export async function get_issue(args: { issue_id: string }, ctx: Ctx): Promise<{ issue: Issue }> {
  return { issue: ((await request(ctx, "GET", `/issues/${seg(args.issue_id)}`)).data ?? {}) as Issue };
}

/**
 * Write: create an issue. `title` and `body_html` are required; `requester_email` links the requester,
 * `account_id` attaches the account, `assignee_id` and `team_id` assign it, `priority` is urgent, high,
 * medium or low, `tags` are tag strings.
 */
export async function create_issue(
  args: {
    title: string;
    body_html: string;
    requester_email?: string;
    account_id?: string;
    assignee_id?: string;
    priority?: string;
    tags?: string[];
    team_id?: string;
  },
  ctx: Ctx,
): Promise<{ issue: Issue }> {
  const payload: Record<string, unknown> = { title: args.title, body_html: args.body_html };
  if (args.requester_email) payload.requester_email = args.requester_email;
  if (args.account_id) payload.account_id = args.account_id;
  if (args.assignee_id) payload.assignee_id = args.assignee_id;
  if (args.priority) payload.priority = args.priority;
  if (args.tags?.length) payload.tags = args.tags;
  if (args.team_id) payload.team_id = args.team_id;
  const body = await request(ctx, "POST", "/issues", { json: payload });
  return { issue: (body.data ?? {}) as Issue };
}

/**
 * Write: update an issue. `state` is new, waiting_on_you, waiting_on_customer, on_hold or closed;
 * `tags` replaces the existing tags; pass an empty string for `assignee_id` or `team_id` to unassign.
 * Only the fields given are sent.
 */
export async function update_issue(
  args: {
    issue_id: string;
    state?: string;
    assignee_id?: string;
    tags?: string[];
    team_id?: string;
    account_id?: string;
  },
  ctx: Ctx,
): Promise<{ issue: Issue }> {
  const payload: Record<string, unknown> = {};
  if (args.state) payload.state = args.state;
  if (args.assignee_id !== undefined) payload.assignee_id = args.assignee_id;
  if (args.tags !== undefined) payload.tags = args.tags;
  if (args.team_id !== undefined) payload.team_id = args.team_id;
  if (args.account_id) payload.account_id = args.account_id;
  const body = await request(ctx, "PATCH", `/issues/${seg(args.issue_id)}`, { json: payload });
  return { issue: (body.data ?? {}) as Issue };
}

/** Delete an issue permanently. */
export async function delete_issue(args: { issue_id: string }, ctx: Ctx): Promise<{ deleted: boolean }> {
  await request(ctx, "DELETE", `/issues/${seg(args.issue_id)}`);
  return { deleted: true };
}

/**
 * Issues matching a Pylon `filter` (see Filter), with the same shape as `list_issues`. `limit` is at most
 * 1000; page with `pagination.cursor` while `pagination.has_next_page`.
 */
export async function search_issues(
  args: { filter: Filter; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ issues: Issue[]; pagination?: Pagination }> {
  const payload: Record<string, unknown> = { filter: args.filter, limit: pageSize(args.limit, 100) };
  if (args.cursor) payload.cursor = args.cursor;
  const body = await request(ctx, "POST", "/issues/search", { json: payload });
  return { issues: Array.isArray(body.data) ? (body.data as Issue[]) : [], pagination: body.pagination };
}

/** Accounts, one page, with name, domains, primary domain, tags and owner. Page with `pagination.cursor`. */
export async function list_accounts(
  args: { limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ accounts: Account[]; pagination?: Pagination }> {
  const body = await request(ctx, "GET", "/accounts", {
    query: { limit: pageSize(args.limit, 100), cursor: args.cursor },
  });
  return { accounts: Array.isArray(body.data) ? (body.data as Account[]) : [], pagination: body.pagination };
}

/** One account by ID or external ID. */
export async function get_account(args: { account_id: string }, ctx: Ctx): Promise<{ account: Account }> {
  return {
    account: ((await request(ctx, "GET", `/accounts/${seg(args.account_id)}`)).data ?? {}) as Account,
  };
}

/** Accounts matching a Pylon `filter`, with the same shape as `list_accounts`. */
export async function search_accounts(
  args: { filter: Filter; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ accounts: Account[]; pagination?: Pagination }> {
  const payload: Record<string, unknown> = { filter: args.filter, limit: pageSize(args.limit, 100) };
  if (args.cursor) payload.cursor = args.cursor;
  const body = await request(ctx, "POST", "/accounts/search", { json: payload });
  return { accounts: Array.isArray(body.data) ? (body.data as Account[]) : [], pagination: body.pagination };
}

/**
 * Write: create an account. `domains` are bare domains (no scheme); `primary_domain` must be one of
 * them; `owner_id` is a Pylon user ID; `tags` are tag strings.
 */
export async function create_account(
  args: {
    name: string;
    domains?: string[];
    primary_domain?: string;
    tags?: string[];
    owner_id?: string;
  },
  ctx: Ctx,
): Promise<{ account: Account }> {
  const payload: Record<string, unknown> = { name: args.name };
  if (args.domains?.length) payload.domains = args.domains;
  if (args.primary_domain) payload.primary_domain = args.primary_domain;
  if (args.tags?.length) payload.tags = args.tags;
  if (args.owner_id) payload.owner_id = args.owner_id;
  const body = await request(ctx, "POST", "/accounts", { json: payload });
  return { account: (body.data ?? {}) as Account };
}

/**
 * Write: update an account. Only the fields given are sent; `tags` replaces the existing tags and an
 * empty string for `owner_id` unassigns the owner.
 */
export async function update_account(
  args: {
    account_id: string;
    name?: string;
    domains?: string[];
    primary_domain?: string;
    tags?: string[];
    owner_id?: string;
  },
  ctx: Ctx,
): Promise<{ account: Account }> {
  const payload: Record<string, unknown> = {};
  if (args.name) payload.name = args.name;
  if (args.domains?.length) payload.domains = args.domains;
  if (args.primary_domain) payload.primary_domain = args.primary_domain;
  if (args.tags !== undefined) payload.tags = args.tags;
  if (args.owner_id !== undefined) payload.owner_id = args.owner_id;
  const body = await request(ctx, "PATCH", `/accounts/${seg(args.account_id)}`, { json: payload });
  return { account: (body.data ?? {}) as Account };
}

/** Contacts, one page, with name, email and account reference. Page with `pagination.cursor`. */
export async function list_contacts(
  args: { limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ contacts: Contact[]; pagination?: Pagination }> {
  const body = await request(ctx, "GET", "/contacts", {
    query: { limit: pageSize(args.limit, 100), cursor: args.cursor },
  });
  return { contacts: Array.isArray(body.data) ? (body.data as Contact[]) : [], pagination: body.pagination };
}

/** One contact by ID: name, email, account. */
export async function get_contact(args: { contact_id: string }, ctx: Ctx): Promise<{ contact: Contact }> {
  return {
    contact: ((await request(ctx, "GET", `/contacts/${seg(args.contact_id)}`)).data ?? {}) as Contact,
  };
}

/** Contacts matching a Pylon `filter`, with the same shape as `list_contacts`. */
export async function search_contacts(
  args: { filter: Filter; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ contacts: Contact[]; pagination?: Pagination }> {
  const payload: Record<string, unknown> = { filter: args.filter, limit: pageSize(args.limit, 100) };
  if (args.cursor) payload.cursor = args.cursor;
  const body = await request(ctx, "POST", "/contacts/search", { json: payload });
  return { contacts: Array.isArray(body.data) ? (body.data as Contact[]) : [], pagination: body.pagination };
}

/** Write: create a contact, optionally with an email and an account to associate it with. */
export async function create_contact(
  args: { name: string; email?: string; account_id?: string },
  ctx: Ctx,
): Promise<{ contact: Contact }> {
  const payload: Record<string, unknown> = { name: args.name };
  if (args.email) payload.email = args.email;
  if (args.account_id) payload.account_id = args.account_id;
  const body = await request(ctx, "POST", "/contacts", { json: payload });
  return { contact: (body.data ?? {}) as Contact };
}

/** Pylon users in the organization, one page: name, email and role. */
export async function list_users(
  args: { limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ users: User[]; pagination?: Pagination }> {
  const body = await request(ctx, "GET", "/users", {
    query: { limit: pageSize(args.limit, 100), cursor: args.cursor },
  });
  return { users: Array.isArray(body.data) ? (body.data as User[]) : [], pagination: body.pagination };
}

/** One user by ID: name, email and role. */
export async function get_user(args: { user_id: string }, ctx: Ctx): Promise<{ user: User }> {
  return { user: ((await request(ctx, "GET", `/users/${seg(args.user_id)}`)).data ?? {}) as User };
}

/** Teams in the organization, with IDs (issues are assigned by `team_id`). */
export async function list_teams(args: {}, ctx: Ctx): Promise<{ teams: Team[] }> {
  const body = await request(ctx, "GET", "/teams");
  return { teams: Array.isArray(body.data) ? (body.data as Team[]) : [] };
}

/** Every tag defined in the organization: value, `object_type` (account, issue or contact) and colour. */
export async function list_tags(args: {}, ctx: Ctx): Promise<{ tags: Tag[] }> {
  const body = await request(ctx, "GET", "/tags");
  return { tags: Array.isArray(body.data) ? (body.data as Tag[]) : [] };
}
