import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Crunchbase's own entity properties, passed through; searches and
// lookups only unwrap the `entities[].properties` envelope. Field names are
// Crunchbase's (https://data.crunchbase.com/docs). Fields the API has no value
// for are absent.

const DEFAULT_BASE = "https://api.crunchbase.com/v4/data";

// Crunchbase returns only the fields a request names.
const ORGANIZATION_FIELDS = [
  "identifier", "short_description", "categories", "location_identifiers", "website_url",
  "linkedin", "twitter", "founded_on", "operating_status", "company_type", "ipo_status",
  "investor_type", "num_employees_enum", "revenue_range", "rank_org", "funding_total",
  "num_funding_rounds", "last_funding_type", "last_funding_at", "num_investments",
];
const ORGANIZATION_DETAIL_FIELDS = [
  "description", "legal_name", "contact_email", "stock_symbol", "valuation", "num_exits",
  "founder_identifiers", "investor_identifiers",
];
const PERSON_FIELDS = [
  "identifier", "first_name", "last_name", "primary_job_title", "primary_organization",
  "short_description", "location_identifiers", "linkedin", "twitter", "website_url", "gender",
  "rank_person", "num_founded_organizations", "num_investments", "num_exits", "num_jobs",
];
const FUNDING_ROUND_FIELDS = [
  "identifier", "short_description", "announced_on", "closed_on", "investment_type",
  "investment_stage", "money_raised", "pre_money_valuation", "post_money_valuation",
  "funded_organization_identifier", "lead_investor_identifiers", "investor_identifiers",
  "num_investors",
];
const INVESTMENT_FIELDS = [
  "identifier", "announced_on", "investor_identifier", "organization_identifier",
  "funding_round_identifier", "funding_round_investment_type", "funding_round_money_raised",
  "money_invested", "is_lead_investor", "partner_identifiers",
];

export type Collection =
  | "organizations" | "organization.companies" | "organization.investors" | "organization.schools"
  | "people" | "funding_rounds" | "acquisitions" | "investments" | "funds" | "events"
  | "categories" | "category_groups" | "locations";

/** A reference to another entity; `value` is its display name. */
export interface Identifier {
  uuid: string;
  value: string;
  permalink?: string;
  entity_def_id: string;
  image_id?: string;
}

export interface Money {
  value: number;
  currency: string;
  value_usd?: number;
}

export interface DateWithPrecision {
  value: string;
  precision: string;
}

export interface Link {
  value: string;
}

export interface Organization {
  identifier: Identifier;
  short_description?: string;
  description?: string;
  legal_name?: string;
  categories?: Identifier[];
  location_identifiers?: Identifier[];
  website_url?: string;
  linkedin?: Link;
  twitter?: Link;
  contact_email?: string;
  founded_on?: DateWithPrecision;
  operating_status?: string;
  company_type?: string;
  ipo_status?: string;
  stock_symbol?: Identifier;
  investor_type?: string[];
  num_employees_enum?: string;
  revenue_range?: string;
  rank_org?: number;
  funding_total?: Money;
  valuation?: Money;
  num_funding_rounds?: number;
  last_funding_type?: string;
  last_funding_at?: string;
  num_investments?: number;
  num_exits?: number;
  founder_identifiers?: Identifier[];
  investor_identifiers?: Identifier[];
}

export interface Person {
  identifier: Identifier;
  first_name?: string;
  last_name?: string;
  primary_job_title?: string;
  primary_organization?: Identifier;
  short_description?: string;
  description?: string;
  location_identifiers?: Identifier[];
  linkedin?: Link;
  twitter?: Link;
  website_url?: string;
  gender?: string;
  rank_person?: number;
  num_founded_organizations?: number;
  num_investments?: number;
  num_exits?: number;
  num_jobs?: number;
}

export interface FundingRound {
  identifier: Identifier;
  short_description?: string;
  announced_on?: string;
  closed_on?: string;
  investment_type?: string;
  investment_stage?: string;
  money_raised?: Money;
  pre_money_valuation?: Money;
  post_money_valuation?: Money;
  funded_organization_identifier?: Identifier;
  lead_investor_identifiers?: Identifier[];
  investor_identifiers?: Identifier[];
  num_investors?: number;
}

export interface Investment {
  identifier: Identifier;
  announced_on?: string;
  investor_identifier?: Identifier;
  organization_identifier?: Identifier;
  funding_round_identifier?: Identifier;
  funding_round_investment_type?: string;
  funding_round_money_raised?: Money;
  money_invested?: Money;
  is_lead_investor?: boolean;
  partner_identifiers?: Identifier[];
}

export interface Suggestion {
  identifier: Identifier;
  short_description?: string;
  facet_ids?: string[];
}

interface Predicate {
  type: "predicate";
  field_id: string;
  operator_id: string;
  values: string[];
}

async function request(ctx: Ctx, path: string, body?: unknown): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.CRUNCHBASE_API_KEY) headers["X-cb-user-key"] = ctx.env.CRUNCHBASE_API_KEY;
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch((ctx.env.CRUNCHBASE_API_URL || DEFAULT_BASE) + path, {
    method: body === undefined ? "GET" : "POST",
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return readJson(res, "Crunchbase");
}

const predicate = (field_id: string, operator_id: string, values: string[]): Predicate => ({
  type: "predicate", field_id, operator_id, values,
});

async function search<T>(
  ctx: Ctx,
  collection: string,
  field_ids: string[],
  predicates: Predicate[],
  order: { field_id: string; sort: "asc" | "desc" },
  page: { limit?: number; after_id?: string },
): Promise<{ count: number; entities: T[] }> {
  const body = await request(ctx, `/searches/${collection}`, {
    field_ids, query: predicates, order: [order], ...page,
  });
  return { count: body.count, entities: body.entities.map((entity: { properties: T }) => entity.properties) };
}

async function entity<T>(ctx: Ctx, collection: string, id: string, field_ids: string[]): Promise<T> {
  const body = await request(ctx, `/entities/${collection}/${encodeURIComponent(id)}${query({ field_ids: field_ids.join(",") })}`);
  return body.properties;
}

function filters(
  includes: [field: string, values: string[] | undefined][],
  announced: { after?: string; before?: string },
): Predicate[] {
  const predicates = includes
    .filter(([, values]) => values?.length)
    .map(([field, values]) => predicate(field, "includes", values!));
  if (announced.after) predicates.push(predicate("announced_on", "gte", [announced.after]));
  if (announced.before) predicates.push(predicate("announced_on", "lte", [announced.before]));
  return predicates;
}

/**
 * Resolve a name to Crunchbase entities (organizations, people, funding rounds, categories,
 * locations and more) with uuid and permalink, most relevant first. `collections` restricts the
 * entity types; `limit` is at most 25.
 */
export async function autocomplete(
  args: { query: string; collections?: Collection[]; limit?: number },
  ctx: Ctx,
): Promise<{ count: number; entities: Suggestion[] }> {
  const params = { query: args.query, collection_ids: args.collections?.join(","), limit: args.limit };
  const { count, entities } = await request(ctx, `/autocompletes${query(params)}`);
  return { count, entities };
}

/**
 * Organizations whose name contains `query`, most prominent first, with description, categories,
 * locations, links, founding date, status, headcount, revenue range, total funding and last round.
 * Page with `after_id` set to the last result's `identifier.uuid`; `limit` is at most 1000.
 */
export async function search_organizations(
  args: { query: string; limit?: number; after_id?: string },
  ctx: Ctx,
): Promise<{ count: number; organizations: Organization[] }> {
  const { count, entities } = await search<Organization>(
    ctx, "organizations", ORGANIZATION_FIELDS, [predicate("identifier", "contains", [args.query])],
    { field_id: "rank_org", sort: "asc" }, { limit: args.limit, after_id: args.after_id },
  );
  return { count, organizations: entities };
}

/**
 * One organization by permalink (for example `stripe`) or uuid: everything in
 * `search_organizations` plus long description, legal name, contact email, stock symbol,
 * latest valuation, exits, founders and investors.
 */
export async function organization_details(
  args: { id: string },
  ctx: Ctx,
): Promise<{ organization: Organization }> {
  const fields = [...ORGANIZATION_FIELDS, ...ORGANIZATION_DETAIL_FIELDS];
  return { organization: await entity<Organization>(ctx, "organizations", args.id, fields) };
}

/**
 * People whose name contains `query`, most prominent first, with title, primary organization,
 * location, links and founder/investor/exit counts. Page with `after_id`; `limit` is at most 1000.
 */
export async function search_people(
  args: { query: string; limit?: number; after_id?: string },
  ctx: Ctx,
): Promise<{ count: number; people: Person[] }> {
  const { count, entities } = await search<Person>(
    ctx, "people", PERSON_FIELDS, [predicate("identifier", "contains", [args.query])],
    { field_id: "rank_person", sort: "asc" }, { limit: args.limit, after_id: args.after_id },
  );
  return { count, people: entities };
}

/** One person by permalink or uuid, with full biography. */
export async function person_details(args: { id: string }, ctx: Ctx): Promise<{ person: Person }> {
  return { person: await entity<Person>(ctx, "people", args.id, [...PERSON_FIELDS, "description"]) };
}

/**
 * Funding rounds, newest first, filtered by funded organizations and/or investors (permalinks or
 * uuids), investment types (seed, series_a, series_b, pre_seed, angel, convertible_note,
 * private_equity, corporate_round, debt_financing, grant, ...) and announced date range
 * (YYYY-MM-DD, inclusive). Returns amount raised, valuations, lead investors and investors.
 */
export async function search_funding_rounds(
  args: {
    organizations?: string[];
    investors?: string[];
    investment_types?: string[];
    announced_after?: string;
    announced_before?: string;
    limit?: number;
    after_id?: string;
  },
  ctx: Ctx,
): Promise<{ count: number; funding_rounds: FundingRound[] }> {
  const predicates = filters(
    [
      ["funded_organization_identifier", args.organizations],
      ["investor_identifiers", args.investors],
      ["investment_type", args.investment_types],
    ],
    { after: args.announced_after, before: args.announced_before },
  );
  const { count, entities } = await search<FundingRound>(
    ctx, "funding_rounds", FUNDING_ROUND_FIELDS, predicates,
    { field_id: "announced_on", sort: "desc" }, { limit: args.limit, after_id: args.after_id },
  );
  return { count, funding_rounds: entities };
}

/** One funding round by uuid or permalink. */
export async function funding_round_details(
  args: { id: string },
  ctx: Ctx,
): Promise<{ funding_round: FundingRound }> {
  return { funding_round: await entity<FundingRound>(ctx, "funding_rounds", args.id, FUNDING_ROUND_FIELDS) };
}

/**
 * Individual investments (one investor in one round), newest first, filtered by investors and/or
 * funded organizations (permalinks or uuids), `lead_only` and announced date range. Use it for an
 * investor's portfolio: round, type and size, amount invested when disclosed, lead flag, partners.
 */
export async function search_investments(
  args: {
    investors?: string[];
    organizations?: string[];
    lead_only?: boolean;
    announced_after?: string;
    announced_before?: string;
    limit?: number;
    after_id?: string;
  },
  ctx: Ctx,
): Promise<{ count: number; investments: Investment[] }> {
  const predicates = filters(
    [
      ["investor_identifier", args.investors],
      ["organization_identifier", args.organizations],
    ],
    { after: args.announced_after, before: args.announced_before },
  );
  if (args.lead_only) predicates.push(predicate("is_lead_investor", "eq", ["true"]));
  const { count, entities } = await search<Investment>(
    ctx, "investments", INVESTMENT_FIELDS, predicates,
    { field_id: "announced_on", sort: "desc" }, { limit: args.limit, after_id: args.after_id },
  );
  return { count, investments: entities };
}
