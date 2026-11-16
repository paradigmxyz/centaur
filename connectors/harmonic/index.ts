import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are Harmonic's own objects, passed through; the interfaces list the
// fields worth knowing. Saved-search results are company or person objects
// depending on the search type.

const DEFAULT_BASE = "https://api.harmonic.ai";

export interface CompanyWebsite {
  domain?: string;
  [field: string]: unknown;
}

export interface Funding {
  /** Total raised, in USD. */
  fundingTotal?: number;
  lastFundingType?: string;
  [field: string]: unknown;
}

export interface Highlight {
  text?: string;
  [field: string]: unknown;
}

export interface Tag {
  displayValue?: string;
  [field: string]: unknown;
}

/** One company. */
export interface Company {
  id?: number | string;
  entity_urn?: string;
  name?: string;
  description?: string;
  website?: CompanyWebsite;
  headcount?: number;
  funding?: Funding;
  foundedDate?: string;
  initialized_date?: string;
  customer_type?: string;
  tags?: (Tag | string)[];
  tagsV2?: (Tag | string)[];
  highlights?: (Highlight | string)[];
  [field: string]: unknown;
}

export interface Experience {
  title?: string;
  role?: string;
  company?: { name?: string; display_name?: string; [field: string]: unknown };
  companyName?: string;
  company_name?: string;
  organizationName?: string;
  is_current?: boolean;
  isCurrent?: boolean;
  current?: boolean;
  end_date?: string;
  endDate?: string;
  [field: string]: unknown;
}

export interface EducationEntry {
  school?: { name?: string; display_name?: string; [field: string]: unknown };
  schoolName?: string;
  school_name?: string;
  degree?: string;
  degree_name?: string;
  field_of_study?: string;
  fieldOfStudy?: string;
  [field: string]: unknown;
}

/** One person. */
export interface Person {
  id?: number | string;
  entity_urn?: string;
  full_name?: string;
  fullName?: string;
  first_name?: string;
  firstName?: string;
  last_name?: string;
  lastName?: string;
  title?: string;
  headline?: string;
  linkedin_headline?: string;
  linkedinHeadline?: string;
  linkedin_url?: string;
  linkedinUrl?: string;
  location?: string | Record<string, unknown>;
  education?: EducationEntry[];
  experience?: Experience[];
  experiences?: Experience[];
  socials?: Record<string, unknown>;
  [field: string]: unknown;
}

export interface SavedSearch {
  id?: number | string;
  urn?: string;
  entity_urn?: string;
  name?: string;
  type?: string;
  entityType?: string;
  count?: number;
  resultCount?: number;
  [field: string]: unknown;
}

/** Harmonic's pagination envelope; `next` is the cursor for the following page. */
export interface PageInfo {
  next?: string;
  [field: string]: unknown;
}

export interface CompanyResults {
  count?: number;
  results?: Company[];
  page_info?: PageInfo;
  [field: string]: unknown;
}

export interface SavedSearchResults {
  count?: number;
  /** Company or person objects, depending on the saved search's type. */
  results?: unknown[];
  page_info?: PageInfo;
  [field: string]: unknown;
}

export interface EnrichmentStatus {
  id?: number | string;
  urn?: string;
  /** Such as COMPLETE, IN_PROGRESS or QUEUED. */
  status?: string;
  enriched_entity_urn?: string;
  [field: string]: unknown;
}

export interface TypeaheadResult {
  name?: string;
  entity_urn?: string;
  id?: number | string;
  description?: string;
  short_description?: string;
  identifier?: { value?: string; [field: string]: unknown };
  [field: string]: unknown;
}

export interface PeopleRecruitingResult {
  /** The saved search that was read. */
  saved_search: { id_or_urn: string; name: string | null; type: string | null };
  applied_filters: {
    role_query: string | null;
    background_query: string | null;
    prior_employers: string[];
    seniority: string[];
    locations: string[];
  };
  /** People kept by the filters out of this page. */
  count: number;
  /** The saved search's total result count. */
  source_count: number;
  page_info: PageInfo;
  /** Raw Harmonic person objects, unfiltered in shape. */
  results: Person[];
}

/** A secret value that may arrive as a multi-line placeholder block; keep its first real line. */
function cleanSecret(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const text = value.trim();
  if (!text.includes("\n")) return text || undefined;
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("===") || trimmed.startsWith("#")) continue;
    return trimmed;
  }
  return undefined;
}

async function request(
  ctx: Ctx,
  method: "GET" | "POST",
  endpoint: string,
  params: Params = {},
  body?: unknown,
): Promise<any> {
  const headers: Record<string, string | undefined> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  headers["apikey"] = cleanSecret(ctx.env.HARMONIC_API_KEY);
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch((ctx.env.HARMONIC_API_URL || DEFAULT_BASE) + endpoint + query(params), {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return readJson(res, "Harmonic");
}

const cleanText = (value: string | null | undefined): string =>
  value ? value.toLowerCase().split(/\s+/).filter(Boolean).join(" ") : "";

function firstNonEmpty(...values: unknown[]): string | null {
  for (const value of values) {
    if (value === null || value === undefined) continue;
    if (typeof value === "string") {
      const trimmed = value.trim();
      if (trimmed) return trimmed;
      continue;
    }
    if (typeof value === "number") return String(value);
  }
  return null;
}

function companyName(experience: Experience | undefined): string | null {
  if (!experience) return null;
  const company = experience.company;
  if (company && typeof company === "object") {
    return firstNonEmpty(company.name, company.display_name);
  }
  return firstNonEmpty(experience.companyName, experience.company_name, experience.organizationName);
}

function experiencesOf(person: Person): Experience[] {
  const value = person.experience ?? person.experiences;
  if (Array.isArray(value)) return value;
  return value && typeof value === "object" ? [value as Experience] : [];
}

function currentExperience(experiences: Experience[]): Experience | undefined {
  for (const experience of experiences) {
    if (experience.is_current === true || experience.isCurrent === true) return experience;
    if (experience.current === true) return experience;
    if (!experience.end_date && !experience.endDate) return experience;
  }
  return experiences[0];
}

function locationText(location: string | Record<string, unknown> | undefined): string | null {
  if (typeof location === "string") return location.trim() || null;
  if (!location || typeof location !== "object") return null;
  const direct = firstNonEmpty(location.display_name, location.displayName, location.formatted, location.name);
  if (direct) return direct;
  const parts = [
    firstNonEmpty(location.city),
    firstNonEmpty(location.region, location.state),
    firstNonEmpty(location.country, location.country_name),
  ];
  return parts.filter(Boolean).join(", ") || null;
}

function dedupe(values: string[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const value of values) {
    const key = cleanText(value);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push(value.split(/\s+/).filter(Boolean).join(" "));
  }
  return out;
}

function educationTerms(education: EducationEntry[] | undefined): string[] {
  const terms: string[] = [];
  for (const entry of Array.isArray(education) ? education : []) {
    if (!entry || typeof entry !== "object") continue;
    const school = entry.school;
    const schoolName =
      school && typeof school === "object"
        ? firstNonEmpty(school.name, school.display_name)
        : firstNonEmpty(entry.schoolName, entry.school_name);
    const degree = firstNonEmpty(entry.degree, entry.degree_name, entry.field_of_study, entry.fieldOfStudy);
    if (schoolName) terms.push(schoolName);
    if (degree) terms.push(degree);
  }
  return dedupe(terms);
}

function matchQuery(haystack: unknown, needle: string | undefined): boolean {
  if (!needle) return true;
  const text = Array.isArray(haystack) ? haystack.map(String).join(" ") : String(haystack ?? "");
  return cleanText(text).includes(cleanText(needle));
}

/** Map a free-form title onto founder, executive, vp, director, manager or individual_contributor. */
function normalizeSeniority(title: string | null): string {
  const text = cleanText(title);
  const tokens = text.split(" ");
  if (!text) return "unknown";
  if (text.includes("founder") || text.includes("cofounder") || text.includes("co-founder")) return "founder";
  if (text.includes("chief of staff")) return "director";
  if (["ceo", "cto", "coo", "cfo", "cmo", "cro", "cio"].some((token) => tokens.includes(token))) return "executive";
  if (text.startsWith("chief ") || text.includes(" president") || text === "president") return "executive";
  if (["svp", "evp"].some((token) => tokens.includes(token)) || text.includes("vice president") || text.includes("head of")) return "vp";
  if (text.includes("director") || text.includes("principal") || text.includes("staff")) return "director";
  if (text.includes("manager") || text.includes("lead")) return "manager";
  return "individual_contributor";
}

function personTitle(person: Person, experiences: Experience[]): string | null {
  const current = currentExperience(experiences);
  return firstNonEmpty(
    current?.title,
    current?.role,
    person.title,
    person.linkedin_headline,
    person.linkedinHeadline,
    person.headline,
  );
}

async function listSavedSearches(ctx: Ctx): Promise<SavedSearch[]> {
  const data = await request(ctx, "GET", "/savedSearches");
  const candidates = Array.isArray(data)
    ? data
    : data && typeof data === "object"
      ? (data.results ?? data.savedSearches ?? [])
      : [];
  return (Array.isArray(candidates) ? candidates : []).filter(
    (item: unknown): item is SavedSearch => Boolean(item) && typeof item === "object",
  );
}

function savedSearchType(search: SavedSearch): string {
  return cleanText(firstNonEmpty(String(search.type ?? ""), String(search.entityType ?? "")) ?? "");
}

function savedSearchId(search: SavedSearch): string | null {
  return firstNonEmpty(search.entity_urn, search.urn, search.id);
}

/** Resolve a people saved search by exact name or unique partial name. */
async function resolvePeopleSavedSearch(ctx: Ctx, name: string): Promise<SavedSearch> {
  const target = cleanText(name);
  if (!target) throw new Error("saved_search_name must not be empty");

  const people = (await listSavedSearches(ctx)).filter((search) => {
    const type = savedSearchType(search);
    return type.includes("people") || type.includes("person");
  });
  const exact = people.filter((search) => cleanText(String(search.name ?? "")) === target);
  if (exact.length === 1) return exact[0];
  const partial = people.filter((search) => cleanText(String(search.name ?? "")).includes(target));
  if (partial.length === 1) return partial[0];
  if (exact.length || partial.length) {
    throw new Error("Multiple people saved searches matched that name. Pass saved_search_id_or_urn instead.");
  }
  throw new Error(`No people saved search matched '${name}'.`);
}

/**
 * Enrich a company from one or more identifiers: `website_domain` (stripe.com), `website_url`,
 * `linkedin_url`, `crunchbase_url`, `pitchbook_url` or `twitter_url`. Returns Harmonic's company
 * object: description, headcount, funding, founded date, tags and highlights.
 */
export async function enrich_company(
  args: {
    website_url?: string;
    website_domain?: string;
    linkedin_url?: string;
    crunchbase_url?: string;
    pitchbook_url?: string;
    twitter_url?: string;
  },
  ctx: Ctx,
): Promise<{ company: Company }> {
  const params: Params = {
    website_url: args.website_url,
    website_domain: args.website_domain,
    linkedin_url: args.linkedin_url,
    crunchbase_url: args.crunchbase_url,
    pitchbook_url: args.pitchbook_url,
    twitter_url: args.twitter_url,
  };
  if (!Object.values(params).some(Boolean)) throw new Error("At least one identifier is required");
  return { company: await request(ctx, "POST", "/companies", params) };
}

/** Enrich a person from their LinkedIn profile URL. Returns Harmonic's person object. */
export async function enrich_person(
  args: { linkedin_url: string },
  ctx: Ctx,
): Promise<{ person: Person }> {
  return { person: await request(ctx, "POST", "/persons", { linkedin_url: args.linkedin_url }) };
}

/**
 * Enrichment status for pending requests, by enrichment `ids` and/or `urns` (comma-joined upstream).
 * Each entry has `id` or `urn`, `status` (COMPLETE, IN_PROGRESS, QUEUED, ...) and, when done,
 * `enriched_entity_urn`.
 */
export async function get_enrichment_status(
  args: { ids?: string[]; urns?: string[] },
  ctx: Ctx,
): Promise<{ statuses: EnrichmentStatus[] }> {
  const data = await request(ctx, "GET", "/enrichment_status", {
    ids: args.ids?.length ? args.ids.join(",") : undefined,
    urns: args.urns?.length ? args.urns.join(",") : undefined,
  });
  const statuses = Array.isArray(data) ? data : (data?.statuses ?? [data]);
  return { statuses: statuses as EnrichmentStatus[] };
}

/**
 * All saved searches the account can see, normalized from Harmonic's listing envelope into
 * `saved_searches`. Each has `name`, an ID (`id` or `urn`/`entity_urn`), a `type` (company or
 * people) and often a result `count`.
 */
export async function saved_searches(args: {}, ctx: Ctx): Promise<{ saved_searches: SavedSearch[] }> {
  return { saved_searches: await listSavedSearches(ctx) };
}

/**
 * One page of a saved search's results, by ID or URN. `size` is the page size (default 50); pass
 * the previous page's `page_info.next` as `cursor`. Results are company or person objects
 * depending on the search.
 */
export async function saved_search_results(
  args: { id_or_urn: string; cursor?: string; size?: number },
  ctx: Ctx,
): Promise<SavedSearchResults> {
  return request(ctx, "GET", `/savedSearches:results/${encodeURIComponent(args.id_or_urn)}`, {
    size: args.size ?? 50,
    cursor: args.cursor,
  });
}

/**
 * One page of results added to a subscribed saved search since you last cleared them. Filter with
 * `new_results_since` (ISO 8601 timestamp) and page with `cursor` / `page_info.next`; `size`
 * defaults to 50.
 */
export async function saved_search_net_new(
  args: { id_or_urn: string; new_results_since?: string; cursor?: string; size?: number },
  ctx: Ctx,
): Promise<SavedSearchResults> {
  return request(ctx, "GET", `/savedSearches:netNewResults/${encodeURIComponent(args.id_or_urn)}`, {
    size: args.size ?? 50,
    new_results_since: args.new_results_since,
    cursor: args.cursor,
  });
}

/** Clear the net-new results of a saved search, so the next read starts from now. */
export async function clear_saved_search_net_new(
  args: { id_or_urn: string },
  ctx: Ctx,
): Promise<Record<string, unknown>> {
  return request(ctx, "POST", `/savedSearches:netNewResults/${encodeURIComponent(args.id_or_urn)}:clear`);
}

/**
 * Natural-language company search (Scout Search), such as "Series A fintech companies in Europe
 * with fewer than 50 employees". `size` is the page size (default 25, up to 1000);
 * `similarity_threshold` is 0.0-1.0; page with `cursor` / `page_info.next`. Returns companies with
 * description, headcount and funding.
 */
export async function search_companies(
  args: { query: string; size?: number; cursor?: string; similarity_threshold?: number },
  ctx: Ctx,
): Promise<CompanyResults> {
  return request(ctx, "GET", "/search/search_agent", {
    query: args.query,
    size: args.size ?? 25,
    cursor: args.cursor,
    similarity_threshold: args.similarity_threshold,
  });
}

/** Companies similar to one company, by numeric ID or URN. `size` defaults to 25. */
export async function similar_companies(
  args: { company_id: string | number; size?: number },
  ctx: Ctx,
): Promise<CompanyResults> {
  return request(ctx, "GET", `/search/similar_companies/${encodeURIComponent(String(args.company_id))}`, {
    size: args.size ?? 25,
  });
}

/**
 * Typeahead lookup by name, domain or a partial match. `search_type` is COMPANY (default), PERSON
 * or INVESTOR; results carry a name, ID/URN and short description.
 */
export async function typeahead(
  args: { query: string; search_type?: "COMPANY" | "PERSON" | "INVESTOR" },
  ctx: Ctx,
): Promise<{ results: TypeaheadResult[] }> {
  const data = await request(ctx, "GET", "/search/typeahead", {
    query: args.query,
    search_type: args.search_type ?? "COMPANY",
  });
  return { results: (data?.results ?? []) as TypeaheadResult[] };
}

/** The account's team connections into one company, by numeric ID or URN. */
export async function company_connections(
  args: { company_id: string | number },
  ctx: Ctx,
): Promise<Record<string, unknown>> {
  return request(ctx, "GET", `/companies/${encodeURIComponent(String(args.company_id))}/userConnections`);
}

/** Create a saved search from `name` and `keywords`. Returns Harmonic's saved-search object. */
export async function create_saved_search(
  args: { name: string; keywords: string },
  ctx: Ctx,
): Promise<SavedSearch> {
  return request(ctx, "POST", "/savedSearches", {}, { name: args.name, keywords: args.keywords });
}

/**
 * Search a people saved search with recruiting filters. `saved_search_id_or_urn` or
 * `saved_search_name` (exact, else unique partial name) picks the search; one page of `size`
 * results (default 25) is read from `cursor`. Filters are case-insensitive substring matches:
 * `role_query` against the current title and headline, `background_query` against company, title,
 * location, prior employers and education; `prior_employers` (any match), `locations` (any match),
 * and `seniority` (any of founder, executive, vp, director, manager, individual_contributor,
 * derived from the current title). Results are raw Harmonic person objects, not rewritten.
 */
export async function search_people_recruiting(
  args: {
    saved_search_id_or_urn?: string;
    saved_search_name?: string;
    role_query?: string;
    background_query?: string;
    prior_employers?: string[];
    seniority?: string[];
    locations?: string[];
    size?: number;
    cursor?: string;
  },
  ctx: Ctx,
): Promise<PeopleRecruitingResult> {
  if (!args.saved_search_id_or_urn && !args.saved_search_name) {
    throw new Error("Pass saved_search_id_or_urn or saved_search_name");
  }

  let savedSearch: SavedSearch | null = null;
  let idOrUrn = args.saved_search_id_or_urn;
  if (!idOrUrn) {
    savedSearch = await resolvePeopleSavedSearch(ctx, args.saved_search_name ?? "");
    idOrUrn = savedSearchId(savedSearch) ?? undefined;
    if (!idOrUrn) throw new Error("Matched people saved search is missing an ID or URN.");
  }

  const data: SavedSearchResults = await request(
    ctx,
    "GET",
    `/savedSearches:results/${encodeURIComponent(idOrUrn)}`,
    { size: args.size ?? 25, cursor: args.cursor },
  );
  const raw = Array.isArray(data.results) ? data.results : [];
  const people = raw.filter((item): item is Person => Boolean(item) && typeof item === "object");

  const seniorityFilters = (args.seniority ?? []).map(cleanText).filter(Boolean);
  const locationFilters = (args.locations ?? []).filter((value) => cleanText(value));
  const employerFilters = (args.prior_employers ?? []).filter((value) => cleanText(value));

  const results: Person[] = [];
  for (const person of people) {
    const experiences = experiencesOf(person);
    const current = currentExperience(experiences);
    const title = personTitle(person, experiences);
    const roleText = [title, firstNonEmpty(person.headline, person.linkedin_headline, person.linkedinHeadline)]
      .filter(Boolean)
      .join(" ");
    const location = locationText(person.location);
    const employers = dedupe(
      experiences
        .filter((experience) => experience !== current)
        .map(companyName)
        .filter((name): name is string => Boolean(name)),
    );
    const backgroundText = [
      companyName(current),
      roleText,
      location,
      employers.join(" "),
      educationTerms(person.education).join(" "),
    ]
      .filter(Boolean)
      .join(" ");

    if (!matchQuery(roleText, args.role_query)) continue;
    if (!matchQuery(backgroundText, args.background_query)) continue;
    if (employerFilters.length && !employerFilters.some((employer) => matchQuery(employers, employer))) continue;
    if (seniorityFilters.length && !seniorityFilters.includes(cleanText(normalizeSeniority(title)))) continue;
    if (locationFilters.length && !locationFilters.some((filter) => matchQuery(location, filter))) continue;
    results.push(person);
  }

  return {
    saved_search: {
      id_or_urn: idOrUrn,
      name: savedSearch ? firstNonEmpty(savedSearch.name) : null,
      type: savedSearch ? firstNonEmpty(savedSearch.type, savedSearch.entityType) : null,
    },
    applied_filters: {
      role_query: args.role_query ?? null,
      background_query: args.background_query ?? null,
      prior_employers: args.prior_employers ?? [],
      seniority: args.seniority ?? [],
      locations: args.locations ?? [],
    },
    count: results.length,
    source_count: data.count ?? people.length,
    page_info: data.page_info ?? {},
    results,
  };
}

/**
 * Escape hatch: call any Harmonic endpoint directly. `method` is GET or POST, `endpoint` a path
 * starting with `/` (for example `/companies`), `params` become the query string and `body` the
 * JSON request body for POST. Returns the upstream JSON as-is.
 */
export async function raw_request(
  args: {
    method?: "GET" | "POST";
    endpoint: string;
    params?: Record<string, string | number | boolean>;
    body?: Record<string, unknown>;
  },
  ctx: Ctx,
): Promise<unknown> {
  const method = (args.method ?? "GET").toUpperCase();
  if (method !== "GET" && method !== "POST") throw new Error(`Unsupported method: ${args.method}`);
  if (!args.endpoint.startsWith("/") || args.endpoint.includes("://")) {
    throw new Error("endpoint must be an API path starting with '/'");
  }
  return request(ctx, method, args.endpoint, args.params ?? {}, args.body);
}
