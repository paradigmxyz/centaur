import type { Ctx } from "../centaur";

const BASE = "https://api.crunchbase.com/v4/data";

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

export interface Identifier {
  uuid: string;
  name: string;
  permalink: string | null;
  entity_type: string;
  url: string | null;
}

export interface Location {
  name: string;
  permalink: string | null;
  location_type: string | null;
}

export interface Money {
  value: number;
  currency: string;
  value_usd: number | null;
}

export interface DateValue {
  date: string;
  precision: string;
}

export interface Organization {
  identifier: Identifier;
  short_description: string | null;
  description: string | null;
  legal_name: string | null;
  categories: Identifier[];
  locations: Location[];
  website_url: string | null;
  linkedin: string | null;
  twitter: string | null;
  contact_email: string | null;
  founded_on: DateValue | null;
  operating_status: string | null;
  company_type: string | null;
  ipo_status: string | null;
  stock_symbol: string | null;
  investor_type: string[];
  num_employees_enum: string | null;
  revenue_range: string | null;
  rank_org: number | null;
  funding_total: Money | null;
  valuation: Money | null;
  num_funding_rounds: number | null;
  last_funding_type: string | null;
  last_funding_at: string | null;
  num_investments: number | null;
  num_exits: number | null;
  founders: Identifier[];
  investors: Identifier[];
}

export interface Person {
  identifier: Identifier;
  first_name: string | null;
  last_name: string | null;
  primary_job_title: string | null;
  primary_organization: Identifier | null;
  short_description: string | null;
  description: string | null;
  locations: Location[];
  linkedin: string | null;
  twitter: string | null;
  website_url: string | null;
  gender: string | null;
  rank_person: number | null;
  num_founded_organizations: number | null;
  num_investments: number | null;
  num_exits: number | null;
  num_jobs: number | null;
}

export interface FundingRound {
  identifier: Identifier;
  short_description: string | null;
  announced_on: string | null;
  closed_on: string | null;
  investment_type: string | null;
  investment_stage: string | null;
  money_raised: Money | null;
  pre_money_valuation: Money | null;
  post_money_valuation: Money | null;
  funded_organization: Identifier | null;
  lead_investors: Identifier[];
  investors: Identifier[];
  num_investors: number | null;
}

export interface Investment {
  identifier: Identifier;
  announced_on: string | null;
  investor: Identifier | null;
  organization: Identifier | null;
  funding_round: Identifier | null;
  funding_round_investment_type: string | null;
  funding_round_money_raised: Money | null;
  money_invested: Money | null;
  is_lead_investor: boolean | null;
  partners: Identifier[];
}

export interface Suggestion {
  identifier: Identifier;
  short_description: string | null;
}

type Raw = Record<string, any>;

function headers(ctx: Ctx): Record<string, string> {
  const out: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  const key = ctx.secret("CRUNCHBASE_API_KEY");
  if (key) out["X-cb-user-key"] = key;
  return out;
}

async function request(ctx: Ctx, path: string, init: { method?: string; body?: unknown } = {}): Promise<Raw> {
  const h = headers(ctx);
  if (init.body !== undefined) h["content-type"] = "application/json";
  const res = await ctx.fetch(BASE + path, {
    method: init.method ?? "GET",
    headers: h,
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Crunchbase returned HTTP ${res.status}: ${text.slice(0, 2000)}`);
  return JSON.parse(text);
}

function query(params: Record<string, string | number | undefined>): string {
  const parts = Object.entries(params)
    .filter(([, value]) => value !== undefined && value !== "")
    .map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`);
  return parts.length ? `?${parts.join("&")}` : "";
}

const str = (value: any): string | null => (typeof value === "string" ? value : null);
const num = (value: any): number | null => (typeof value === "number" ? value : null);
const link = (value: any): string | null => str(value?.value);

function identifier(raw: Raw | null | undefined): Identifier | null {
  if (!raw) return null;
  return {
    uuid: raw.uuid,
    name: raw.value ?? raw.name ?? "",
    permalink: str(raw.permalink),
    entity_type: raw.entity_def_id ?? raw.entity_type ?? "",
    url: str(raw.url),
  };
}

const identifiers = (raw: any): Identifier[] =>
  Array.isArray(raw) ? raw.map(identifier).filter((id): id is Identifier => id !== null) : [];

const locations = (raw: any): Location[] =>
  Array.isArray(raw)
    ? raw.map((l: Raw) => ({ name: l.value ?? l.name ?? "", permalink: str(l.permalink), location_type: str(l.location_type) }))
    : [];

function money(raw: any): Money | null {
  if (!raw || typeof raw.value !== "number") return null;
  return { value: raw.value, currency: raw.currency, value_usd: num(raw.value_usd) };
}

function date(raw: any): DateValue | null {
  if (!raw) return null;
  return { date: raw.value ?? raw.date, precision: raw.precision };
}

function organization(p: Raw): Organization {
  return {
    identifier: identifier(p.identifier)!,
    short_description: str(p.short_description),
    description: str(p.description),
    legal_name: str(p.legal_name),
    categories: identifiers(p.categories),
    locations: locations(p.location_identifiers),
    website_url: str(p.website_url),
    linkedin: link(p.linkedin),
    twitter: link(p.twitter),
    contact_email: str(p.contact_email),
    founded_on: date(p.founded_on),
    operating_status: str(p.operating_status),
    company_type: str(p.company_type),
    ipo_status: str(p.ipo_status),
    stock_symbol: link(p.stock_symbol),
    investor_type: Array.isArray(p.investor_type) ? p.investor_type : [],
    num_employees_enum: str(p.num_employees_enum),
    revenue_range: str(p.revenue_range),
    rank_org: num(p.rank_org),
    funding_total: money(p.funding_total),
    valuation: money(p.valuation),
    num_funding_rounds: num(p.num_funding_rounds),
    last_funding_type: str(p.last_funding_type),
    last_funding_at: str(p.last_funding_at),
    num_investments: num(p.num_investments),
    num_exits: num(p.num_exits),
    founders: identifiers(p.founder_identifiers),
    investors: identifiers(p.investor_identifiers),
  };
}

function person(p: Raw): Person {
  return {
    identifier: identifier(p.identifier)!,
    first_name: str(p.first_name),
    last_name: str(p.last_name),
    primary_job_title: str(p.primary_job_title),
    primary_organization: identifier(p.primary_organization),
    short_description: str(p.short_description),
    description: str(p.description),
    locations: locations(p.location_identifiers),
    linkedin: link(p.linkedin),
    twitter: link(p.twitter),
    website_url: str(p.website_url),
    gender: str(p.gender),
    rank_person: num(p.rank_person),
    num_founded_organizations: num(p.num_founded_organizations),
    num_investments: num(p.num_investments),
    num_exits: num(p.num_exits),
    num_jobs: num(p.num_jobs),
  };
}

function fundingRound(p: Raw): FundingRound {
  return {
    identifier: identifier(p.identifier)!,
    short_description: str(p.short_description),
    announced_on: str(p.announced_on),
    closed_on: str(p.closed_on),
    investment_type: str(p.investment_type),
    investment_stage: str(p.investment_stage),
    money_raised: money(p.money_raised),
    pre_money_valuation: money(p.pre_money_valuation),
    post_money_valuation: money(p.post_money_valuation),
    funded_organization: identifier(p.funded_organization_identifier),
    lead_investors: identifiers(p.lead_investor_identifiers),
    investors: identifiers(p.investor_identifiers),
    num_investors: num(p.num_investors),
  };
}

function investment(p: Raw): Investment {
  return {
    identifier: identifier(p.identifier)!,
    announced_on: str(p.announced_on),
    investor: identifier(p.investor_identifier),
    organization: identifier(p.organization_identifier),
    funding_round: identifier(p.funding_round_identifier),
    funding_round_investment_type: str(p.funding_round_investment_type),
    funding_round_money_raised: money(p.funding_round_money_raised),
    money_invested: money(p.money_invested),
    is_lead_investor: typeof p.is_lead_investor === "boolean" ? p.is_lead_investor : null,
    partners: identifiers(p.partner_identifiers),
  };
}

interface Predicate {
  type: "predicate";
  field_id: string;
  operator_id: string;
  values: string[];
}

const predicate = (field_id: string, operator_id: string, values: string[]): Predicate => ({
  type: "predicate", field_id, operator_id, values,
});

async function search(
  ctx: Ctx,
  collection: string,
  field_ids: string[],
  predicates: Predicate[],
  order: { field_id: string; sort: "asc" | "desc" },
  limit?: number,
  after_id?: string,
): Promise<{ count: number; entities: Raw[] }> {
  const body: Raw = { field_ids, query: predicates, order: [order] };
  if (limit !== undefined) body.limit = limit;
  if (after_id !== undefined) body.after_id = after_id;
  const page = await request(ctx, `/searches/${collection}`, { method: "POST", body });
  return { count: page.count ?? 0, entities: (page.entities ?? []).map((e: Raw) => e.properties) };
}

async function entity(ctx: Ctx, collection: string, id: string, field_ids: string[]): Promise<Raw> {
  const page = await request(
    ctx,
    `/entities/${collection}/${encodeURIComponent(id)}${query({ field_ids: field_ids.join(",") })}`,
  );
  return page.properties ?? {};
}

function dateRange(predicates: Predicate[], field: string, after?: string, before?: string): void {
  if (after) predicates.push(predicate(field, "gte", [after]));
  if (before) predicates.push(predicate(field, "lte", [before]));
}

function includes(predicates: Predicate[], field: string, values?: string[]): void {
  if (values && values.length) predicates.push(predicate(field, "includes", values));
}

/**
 * Resolve a name to Crunchbase entities (organizations, people, funding rounds, categories,
 * locations and more) with uuid and permalink, most relevant first. `collections` restricts the
 * entity types; `limit` is at most 25.
 */
export async function autocomplete(
  args: { query: string; collections?: Collection[]; limit?: number },
  ctx: Ctx,
): Promise<{ count: number; suggestions: Suggestion[] }> {
  const page = await request(
    ctx,
    `/autocompletes${query({ query: args.query, collection_ids: args.collections?.join(","), limit: args.limit })}`,
  );
  return {
    count: page.count ?? 0,
    suggestions: (page.entities ?? []).map((e: Raw) => ({
      identifier: identifier(e.identifier)!,
      short_description: str(e.short_description),
    })),
  };
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
  const page = await search(
    ctx, "organizations", ORGANIZATION_FIELDS,
    [predicate("identifier", "contains", [args.query])],
    { field_id: "rank_org", sort: "asc" }, args.limit, args.after_id,
  );
  return { count: page.count, organizations: page.entities.map(organization) };
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
  const raw = await entity(ctx, "organizations", args.id, [...ORGANIZATION_FIELDS, ...ORGANIZATION_DETAIL_FIELDS]);
  return { organization: organization(raw) };
}

/**
 * People whose name contains `query`, most prominent first, with title, primary organization,
 * location, links and founder/investor/exit counts. Page with `after_id`; `limit` is at most 1000.
 */
export async function search_people(
  args: { query: string; limit?: number; after_id?: string },
  ctx: Ctx,
): Promise<{ count: number; people: Person[] }> {
  const page = await search(
    ctx, "people", PERSON_FIELDS,
    [predicate("identifier", "contains", [args.query])],
    { field_id: "rank_person", sort: "asc" }, args.limit, args.after_id,
  );
  return { count: page.count, people: page.entities.map(person) };
}

/** One person by permalink or uuid, with full biography. */
export async function person_details(args: { id: string }, ctx: Ctx): Promise<{ person: Person }> {
  const raw = await entity(ctx, "people", args.id, [...PERSON_FIELDS, "description"]);
  return { person: person(raw) };
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
  const predicates: Predicate[] = [];
  includes(predicates, "funded_organization_identifier", args.organizations);
  includes(predicates, "investor_identifiers", args.investors);
  includes(predicates, "investment_type", args.investment_types);
  dateRange(predicates, "announced_on", args.announced_after, args.announced_before);
  const page = await search(
    ctx, "funding_rounds", FUNDING_ROUND_FIELDS, predicates,
    { field_id: "announced_on", sort: "desc" }, args.limit, args.after_id,
  );
  return { count: page.count, funding_rounds: page.entities.map(fundingRound) };
}

/** One funding round by uuid or permalink. */
export async function funding_round_details(
  args: { id: string },
  ctx: Ctx,
): Promise<{ funding_round: FundingRound }> {
  const raw = await entity(ctx, "funding_rounds", args.id, FUNDING_ROUND_FIELDS);
  return { funding_round: fundingRound(raw) };
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
  const predicates: Predicate[] = [];
  includes(predicates, "investor_identifier", args.investors);
  includes(predicates, "organization_identifier", args.organizations);
  if (args.lead_only) predicates.push(predicate("is_lead_investor", "eq", ["true"]));
  dateRange(predicates, "announced_on", args.announced_after, args.announced_before);
  const page = await search(
    ctx, "investments", INVESTMENT_FIELDS, predicates,
    { field_id: "announced_on", sort: "desc" }, args.limit, args.after_id,
  );
  return { count: page.count, investments: page.entities.map(investment) };
}
