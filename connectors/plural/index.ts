import type { Ctx } from "../centaur";
import { encode, readJson, type Params } from "../_lib/http";

// Results are Open States' own objects, passed through. The interfaces list the
// fields worth knowing; Open States may send more.

const BASE = "https://v3.openstates.org";

/** Pagination metadata returned with list endpoints. */
export interface Pagination {
  per_page: number;
  page: number;
  max_page: number;
  total_items: number;
  [field: string]: unknown;
}

/** A paginated Open States response. */
export interface Paginated<T> {
  results: T[];
  pagination?: Pagination;
  [field: string]: unknown;
}

/** A jurisdiction or organization reference, such as a state or legislative chamber. */
export interface OrganizationRef {
  id?: string;
  name?: string;
  classification?: string;
  [field: string]: unknown;
}

/** A state, territory or municipality. */
export interface Jurisdiction {
  id: string;
  name: string;
  classification?: string;
  division_id?: string;
  url?: string;
  [field: string]: unknown;
}

/** A legislator's current office. */
export interface CurrentRole {
  title?: string;
  org_classification?: string;
  district?: string;
  party?: string;
  jurisdiction?: string;
  [field: string]: unknown;
}

/** A legislator, governor or other person in state government. */
export interface Person {
  id: string;
  name: string;
  party?: string;
  current_role?: CurrentRole;
  jurisdiction?: OrganizationRef;
  [field: string]: unknown;
}

/** A bill or resolution. */
export interface Bill {
  id: string;
  identifier: string;
  title: string;
  session?: string;
  jurisdiction?: OrganizationRef;
  updated_at?: string;
  [field: string]: unknown;
}

/** A legislative committee. */
export interface Committee {
  id: string;
  name: string;
  classification?: string;
  parent?: OrganizationRef;
  [field: string]: unknown;
}

/** Where an event takes place. */
export interface EventLocation {
  name?: string;
  [field: string]: unknown;
}

/** A hearing, floor session or other legislative event. */
export interface Event {
  id: string;
  name: string;
  start_date?: string;
  location?: EventLocation;
  jurisdiction?: OrganizationRef;
  [field: string]: unknown;
}

function includes(include?: string[]): string {
  return (include ?? [])
    .map((value) => `include=${encodeURIComponent(value)}`)
    .join("&");
}

function query(params: Params, include?: string[]): string {
  const base = encode(params);
  const extra = includes(include);
  const search = [base, extra].filter(Boolean).join("&");
  return search ? `?${search}` : "";
}

async function request<T>(
  ctx: Ctx,
  path: string,
  params: Params = {},
  include?: string[],
): Promise<T> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.PLURAL_API_KEY) headers["X-API-KEY"] = ctx.env.PLURAL_API_KEY;
  const res = await ctx.fetch(`${BASE}${path}${query(params, include)}`, { headers });
  return readJson(res, "Open States");
}

/**
 * List available jurisdictions (states, territories, municipalities), with OCD id, name and
 * classification. `classification` is `state` or `municipality`; `include` adds `organizations`,
 * `legislative_sessions` or `latest_runs`. Page with `page` (starts at 1) and `per_page` (default
 * 52).
 */
export async function list_jurisdictions(
  args: { classification?: string; include?: string[]; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<Paginated<Jurisdiction>> {
  return request<Paginated<Jurisdiction>>(
    ctx,
    "/jurisdictions",
    {
      classification: args.classification,
      page: args.page ?? 1,
      per_page: args.per_page ?? 52,
    },
    args.include,
  );
}

/**
 * Get one jurisdiction by name or OCD id (for example
 * `ocd-jurisdiction/country:us/state:ca/government`). `include` adds `organizations`,
 * `legislative_sessions` or `latest_runs`.
 */
export async function get_jurisdiction(
  args: { jurisdiction_id: string; include?: string[] },
  ctx: Ctx,
): Promise<{ jurisdiction: Jurisdiction }> {
  return {
    jurisdiction: await request<Jurisdiction>(
      ctx,
      `/jurisdictions/${args.jurisdiction_id}`,
      {},
      args.include,
    ),
  };
}

/**
 * Search legislators, governors and other officials. Provide `jurisdiction` (state name,
 * abbreviation or OCD id) and/or `name`. `org_classification` is `upper`, `lower`, `legislature`
 * or `executive`; `district` filters by district name. `include` adds `other_names`,
 * `other_identifiers`, `links`, `sources` or `offices`. Page with `page` and `per_page` (default
 * 10).
 */
export async function search_people(
  args: {
    jurisdiction?: string;
    name?: string;
    org_classification?: string;
    district?: string;
    include?: string[];
    page?: number;
    per_page?: number;
  },
  ctx: Ctx,
): Promise<Paginated<Person>> {
  if (!args.jurisdiction && !args.name) {
    throw new Error("search_people needs a jurisdiction or a name");
  }
  return request<Paginated<Person>>(
    ctx,
    "/people",
    {
      jurisdiction: args.jurisdiction,
      name: args.name,
      org_classification: args.org_classification,
      district: args.district,
      page: args.page ?? 1,
      per_page: args.per_page ?? 10,
    },
    args.include,
  );
}

/**
 * Get the legislators whose district contains a latitude/longitude. `include` adds `other_names`,
 * `other_identifiers`, `links`, `sources` or `offices`.
 */
export async function people_by_location(
  args: { lat: number; lng: number; include?: string[] },
  ctx: Ctx,
): Promise<Paginated<Person>> {
  return request<Paginated<Person>>(ctx, "/people.geo", { lat: args.lat, lng: args.lng }, args.include);
}

/**
 * Search bills by jurisdiction, session, chamber, classification, subject, full-text `q`, sponsor,
 * updated/created/action date (ISO 8601), with `sort` one of `updated_desc`, `updated_asc`,
 * `first_action_desc`, `first_action_asc`, `latest_action_desc`, `latest_action_asc` (default
 * `updated_desc`). `include` adds `sponsorships`, `abstracts`, `other_titles`, `other_identifiers`,
 * `actions`, `sources`, `documents`, `versions`, `votes` or `related_bills`. Page with `page` and
 * `per_page` (default 10).
 */
export async function search_bills(
  args: {
    jurisdiction?: string;
    session?: string;
    chamber?: string;
    classification?: string;
    subject?: string;
    q?: string;
    sponsor?: string;
    updated_since?: string;
    created_since?: string;
    action_since?: string;
    sort?: string;
    include?: string[];
    page?: number;
    per_page?: number;
  },
  ctx: Ctx,
): Promise<Paginated<Bill>> {
  return request<Paginated<Bill>>(
    ctx,
    "/bills",
    {
      jurisdiction: args.jurisdiction,
      session: args.session,
      chamber: args.chamber,
      classification: args.classification,
      subject: args.subject,
      q: args.q,
      sponsor: args.sponsor,
      updated_since: args.updated_since,
      created_since: args.created_since,
      action_since: args.action_since,
      sort: args.sort ?? "updated_desc",
      page: args.page ?? 1,
      per_page: args.per_page ?? 10,
    },
    args.include,
  );
}

/**
 * Get one bill by jurisdiction (state name or abbreviation), legislative `session` and `bill_id`
 * (for example `HB 1`). `include` adds `sponsorships`, `abstracts`, `other_titles`,
 * `other_identifiers`, `actions`, `sources`, `documents`, `versions`, `votes` or `related_bills`.
 */
export async function get_bill(
  args: { jurisdiction: string; session: string; bill_id: string; include?: string[] },
  ctx: Ctx,
): Promise<{ bill: Bill }> {
  const path = `/bills/${args.jurisdiction}/${encodeURIComponent(args.session)}/${encodeURIComponent(args.bill_id)}`;
  return { bill: await request<Bill>(ctx, path, {}, args.include) };
}

/**
 * Get a bill by its Open States internal UUID. `include` adds `sponsorships`, `abstracts`,
 * `other_titles`, `other_identifiers`, `actions`, `sources`, `documents`, `versions`, `votes` or
 * `related_bills`.
 */
export async function get_bill_by_id(
  args: { openstates_bill_id: string; include?: string[] },
  ctx: Ctx,
): Promise<{ bill: Bill }> {
  const path = `/bills/ocd-bill/${encodeURIComponent(args.openstates_bill_id)}`;
  return { bill: await request<Bill>(ctx, path, {}, args.include) };
}

/**
 * List committees for a jurisdiction (state name, abbreviation or OCD id), optionally filtered by
 * `chamber` (`upper` or `lower`). `include` adds `memberships`, `links` or `sources`. Page with
 * `page` and `per_page` (default 20).
 */
export async function list_committees(
  args: {
    jurisdiction: string;
    chamber?: string;
    include?: string[];
    page?: number;
    per_page?: number;
  },
  ctx: Ctx,
): Promise<Paginated<Committee>> {
  return request<Paginated<Committee>>(
    ctx,
    "/committees",
    {
      jurisdiction: args.jurisdiction,
      chamber: args.chamber,
      page: args.page ?? 1,
      per_page: args.per_page ?? 20,
    },
    args.include,
  );
}

/**
 * Get one committee by its OCD id. `include` adds `memberships`, `links` or `sources`.
 */
export async function get_committee(
  args: { committee_id: string; include?: string[] },
  ctx: Ctx,
): Promise<{ committee: Committee }> {
  return {
    committee: await request<Committee>(
      ctx,
      `/committees/${args.committee_id}`,
      {},
      args.include,
    ),
  };
}

/**
 * List legislative events such as hearings and floor sessions, optionally filtered by
 * `jurisdiction` and starting before/after ISO 8601 datetimes. `require_bills` keeps only events
 * with associated bills. `include` adds `links`, `sources`, `media`, `documents`, `participants` or
 * `agenda`. Page with `page` and `per_page` (default 20).
 */
export async function list_events(
  args: {
    jurisdiction?: string;
    before?: string;
    after?: string;
    require_bills?: boolean;
    include?: string[];
    page?: number;
    per_page?: number;
  },
  ctx: Ctx,
): Promise<Paginated<Event>> {
  return request<Paginated<Event>>(
    ctx,
    "/events",
    {
      jurisdiction: args.jurisdiction,
      before: args.before,
      after: args.after,
      require_bills: args.require_bills ? true : undefined,
      page: args.page ?? 1,
      per_page: args.per_page ?? 20,
    },
    args.include,
  );
}

/**
 * Get one legislative event by its OCD id. `include` adds `links`, `sources`, `media`, `documents`,
 * `participants` or `agenda`.
 */
export async function get_event(
  args: { event_id: string; include?: string[] },
  ctx: Ctx,
): Promise<{ event: Event }> {
  return { event: await request<Event>(ctx, `/events/${args.event_id}`, {}, args.include) };
}
