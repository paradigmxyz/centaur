import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are People API `Person` objects, passed through: `names[].displayName`,
// `emailAddresses[].value`, `phoneNumbers[].value`, `organizations[]`, `photos[]`. Envelopes
// (`people`, `results[].person`) are unwrapped and nothing is renamed; read only what the
// readMask or personFields you ask for requests, so wide masks cost more data.
// Reference: https://developers.google.com/people/api/rest. Authentication is iron-proxy's
// oauth_token transform, which mints an access token from GOOGLE_TOKEN_JSON and injects the
// Authorization header on the wire; the connector itself sends no credentials unless
// GOOGLE_ACCESS_TOKEN is set (local testing only).

const PEOPLE = "https://people.googleapis.com/v1";
const DIRECTORY_SOURCE = "DIRECTORY_SOURCE_TYPE_DOMAIN_PROFILE";

/** Name fields requested by default, from the Workspace directory and contacts. */
const DIRECTORY_READ_MASK = "names,emailAddresses";

/** Contacts fields requested by default; add organizations, addresses, birthdays, photos, ... */
const CONTACTS_PERSON_FIELDS = "names,emailAddresses,phoneNumbers,organizations";

/** A display name. `metadata.primary` marks the one the person goes by. */
export interface PersonName {
  displayName?: string;
  displayNameLastFirst?: string;
  givenName?: string;
  familyName?: string;
  unstructuredName?: string;
  metadata?: { primary?: boolean; source?: PersonSource; [field: string]: unknown };
  [field: string]: unknown;
}

/** An email address; `value` is the address, `type` a label such as `work` or `home`. */
export interface PersonEmail {
  value?: string;
  type?: string;
  formattedType?: string;
  metadata?: { primary?: boolean; source?: PersonSource; [field: string]: unknown };
  [field: string]: unknown;
}

/** A phone number; `value` is the number as stored (no normalization applied). */
export interface PersonPhone {
  value?: string;
  canonicalForm?: string;
  type?: string;
  formattedType?: string;
  metadata?: { primary?: boolean; source?: PersonSource; [field: string]: unknown };
  [field: string]: unknown;
}

/** An organization the person is or was part of. */
export interface PersonOrganization {
  name?: string;
  title?: string;
  department?: string;
  type?: string;
  current?: boolean;
  startDate?: { year?: number; month?: number; day?: number; [field: string]: unknown };
  endDate?: { year?: number; month?: number; day?: number; [field: string]: unknown };
  metadata?: { primary?: boolean; source?: PersonSource; [field: string]: unknown };
  [field: string]: unknown;
}

/** A profile photo URL. */
export interface PersonPhoto {
  url?: string;
  default?: boolean;
  metadata?: { primary?: boolean; source?: PersonSource; [field: string]: unknown };
  [field: string]: unknown;
}

/** Where a field came from: the contact itself, a domain profile, a profile, or "other contact". */
export interface PersonSource {
  type?: string;
  id?: string;
  etag?: string;
  [field: string]: unknown;
}

/**
 * A People API person. Only the fields listed in the readMask/personFields are present; fetch
 * again with a wider mask for more. `resourceName` is `people/…` or `otherContacts/…`.
 */
export interface Person {
  resourceName: string;
  etag?: string;
  names?: PersonName[];
  emailAddresses?: PersonEmail[];
  phoneNumbers?: PersonPhone[];
  organizations?: PersonOrganization[];
  photos?: PersonPhoto[];
  metadata?: { sources?: PersonSource[]; objectType?: string; [field: string]: unknown };
  [field: string]: unknown;
}

function headers(ctx: Ctx): Record<string, string> {
  const out: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the access token is injected on the wire; locally it may come from the env.
  if (ctx.env.GOOGLE_ACCESS_TOKEN) out.authorization = `Bearer ${ctx.env.GOOGLE_ACCESS_TOKEN}`;
  return out;
}

/** A People REST call; throws with Google's own error message on a non-2xx response. */
async function people(ctx: Ctx, path: string, params: Params = {}): Promise<any> {
  const res = await ctx.fetch(`${PEOPLE}${path}${query(params)}`, { headers: headers(ctx) });
  return readJson(res, "People");
}

/**
 * A resource name usable as a path segment: `people/c123` stays as is, a bare `c123` becomes
 * `people/c123`, and a full `https://people.googleapis.com/v1/people/c123` URL is reduced to its
 * resource name.
 */
function resourceName(value: string): string {
  const raw = value.trim().replace(/^https?:\/\/[^/]+\/v1\//, "").replace(/^\/+/, "");
  if (raw.startsWith("people/") || raw.startsWith("otherContacts/")) return raw;
  return `people/${raw}`;
}

/**
 * Everyone visible in the Workspace domain directory, with names and email addresses by default.
 * Pages through the whole directory (`page_size` up to 1000 per request); `max_results` stops
 * early, and `page_token` resumes from a previous `next_page_token`. `read_mask` requests more
 * fields (for example `names,emailAddresses,phoneNumbers,organizations`).
 */
export async function directory_list(
  args: { max_results?: number; page_size?: number; page_token?: string; read_mask?: string },
  ctx: Ctx,
): Promise<{ people: Person[]; next_page_token: string | null }> {
  const pageSize = Math.min(Math.max(args.page_size ?? 1000, 1), 1000);
  const readMask = args.read_mask ?? DIRECTORY_READ_MASK;
  const found: Person[] = [];
  let pageToken = args.page_token;
  for (;;) {
    const body = await people(ctx, "/people:listDirectoryPeople", {
      readMask,
      sources: DIRECTORY_SOURCE,
      pageSize,
      pageToken,
    });
    found.push(...((body.people ?? []) as Person[]));
    pageToken = body.nextPageToken;
    if (!pageToken || (args.max_results !== undefined && found.length >= args.max_results)) break;
  }
  return {
    people: args.max_results === undefined ? found : found.slice(0, args.max_results),
    next_page_token: pageToken ?? null,
  };
}

/**
 * Prefix search of the Workspace domain directory by name or email (`query` is the prefix, as the
 * People API matches it); returns matching people with names and email addresses. `max_results`
 * defaults to 20 and caps how many people come back; results are not ranked, so search a longer
 * prefix when a short one is noisy.
 */
export async function directory_search(
  args: { query: string; max_results?: number; page_size?: number; read_mask?: string },
  ctx: Ctx,
): Promise<{ people: Person[]; next_page_token: string | null }> {
  const maxResults = Math.max(args.max_results ?? 20, 1);
  const readMask = args.read_mask ?? DIRECTORY_READ_MASK;
  const found: Person[] = [];
  let pageToken: string | undefined;
  for (;;) {
    const body = await people(ctx, "/people:searchDirectoryPeople", {
      query: args.query,
      readMask,
      sources: DIRECTORY_SOURCE,
      pageSize: Math.min(args.page_size ?? maxResults, 500),
      pageToken,
    });
    found.push(...((body.people ?? []) as Person[]));
    pageToken = body.nextPageToken;
    if (!pageToken || found.length >= maxResults) break;
  }
  return { people: found.slice(0, maxResults), next_page_token: pageToken ?? null };
}

/**
 * The user's own contacts (`people/me/connections`), newest-modified first by default. `total_people`
 * is the size of the contact list; follow `next_page_token` as `page_token` to page (100 per page
 * is the maximum). `person_fields` adds fields beyond names, emails, phones and organizations, for
 * example `addresses,birthdays,memberships,photos`. A `sync_token` (from `next_sync_token`) returns
 * only what changed since, and cannot be combined with a page token or sort order; the other
 * arguments must match the request that produced it. Requires a sync-token-capable scope.
 */
export async function list_contacts(
  args: {
    page_size?: number;
    page_token?: string;
    person_fields?: string;
    sort_order?: "LAST_MODIFIED_ASCENDING" | "LAST_MODIFIED_DESCENDING" | "FIRST_NAME_ASCENDING" | "LAST_NAME_ASCENDING";
    sync_token?: string;
  },
  ctx: Ctx,
): Promise<{ people: Person[]; next_page_token: string | null; next_sync_token: string | null; total_people: number }> {
  const body = await people(ctx, "/people/me/connections", {
    personFields: args.person_fields ?? CONTACTS_PERSON_FIELDS,
    pageSize: Math.min(Math.max(args.page_size ?? 100, 1), 1000),
    pageToken: args.sync_token ? undefined : args.page_token,
    sortOrder: args.sync_token ? undefined : (args.sort_order ?? "LAST_MODIFIED_DESCENDING"),
    syncToken: args.sync_token,
    sources: "READ_SOURCE_TYPE_CONTACT",
  });
  return {
    people: body.connections ?? [],
    next_page_token: body.nextPageToken ?? null,
    next_sync_token: body.nextSyncToken ?? null,
    total_people: body.totalPeople ?? 0,
  };
}

/**
 * Search the user's own contacts and domain profiles by name, email or phone. The API returns at
 * most 30 matches and does not paginate; `read_mask` defaults to names, emails, phones and
 * organizations. Matches are relevance-ordered. Other contacts are not included: use
 * `list_other_contacts` and filter.
 */
export async function search_contacts(
  args: { query: string; read_mask?: string; page_size?: number },
  ctx: Ctx,
): Promise<{ people: Person[] }> {
  const body = await people(ctx, "/people:searchContacts", {
    query: args.query,
    readMask: args.read_mask ?? CONTACTS_PERSON_FIELDS,
    pageSize: Math.min(Math.max(args.page_size ?? 30, 1), 30),
  });
  return { people: ((body.results ?? []) as { person: Person }[]).map((result) => result.person) };
}

/**
 * One person by resource name (`people/c123`, a bare `c123`, or a full People API URL) with the
 * fields named in `person_fields` (default names, emails, phones and organizations).
 */
export async function get_person(
  args: { resource_name: string; person_fields?: string },
  ctx: Ctx,
): Promise<{ person: Person }> {
  const path = `/${resourceName(args.resource_name)}`;
  return { person: await people(ctx, path, { personFields: args.person_fields ?? CONTACTS_PERSON_FIELDS }) };
}

/**
 * "Other contacts": people Gmail auto-collected from mail you sent or received, who are not in your
 * contacts. Useful for resolving an address that is not in the directory. Page with `page_token`
 * (1000 per page maximum); `read_mask` defaults to names, emails, phones and organizations. Needs
 * the otherContacts read scope, so it is unavailable when the credential was not consented for it.
 */
export async function list_other_contacts(
  args: { page_size?: number; page_token?: string; read_mask?: string; sync_token?: string },
  ctx: Ctx,
): Promise<{ people: Person[]; next_page_token: string | null; next_sync_token: string | null; total_people: number }> {
  const body = await people(ctx, "/otherContacts", {
    readMask: args.read_mask ?? CONTACTS_PERSON_FIELDS,
    pageSize: Math.min(Math.max(args.page_size ?? 100, 1), 1000),
    pageToken: args.sync_token ? undefined : args.page_token,
    syncToken: args.sync_token,
    sources: "READ_SOURCE_TYPE_OTHER_CONTACT",
  });
  return {
    people: body.otherContacts ?? [],
    next_page_token: body.nextPageToken ?? null,
    next_sync_token: body.nextSyncToken ?? null,
    total_people: body.totalPeople ?? 0,
  };
}
