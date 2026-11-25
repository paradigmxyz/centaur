import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// List endpoints return LegiStorm's own response body, including any envelope
// and pagination fields it sends; row types below name the fields worth knowing.

const DEFAULT_BASE = "https://api.legistorm.com/v2.019.10/congress";

const ISSUE_ENDPOINT_CANDIDATES = [
  "/issue_staff/list",
  "/member/issue/list",
  "/office/issue/list",
  "/committee/issue/list",
  "/staff/issue/list",
];

/** A position a staffer holds. */
export interface Position {
  position_title?: string;
  is_current?: boolean;
  member?: { member_id?: number; [field: string]: unknown };
  office?: { office_id?: number; [field: string]: unknown };
  [field: string]: unknown;
}

/** One email address attached to a staff row. */
export interface StaffEmail {
  contact_string?: string;
  [field: string]: unknown;
}

/** An address tied to a staffer's current or past office. */
export interface OfficeMemberAddress {
  office?: { office_id?: number; [field: string]: unknown };
  member?: { member_id?: number; [field: string]: unknown };
  [field: string]: unknown;
}

/** A member row. */
export interface Member {
  member_id?: number;
  id?: number;
  first_name?: string;
  last_name?: string;
  preferred_first_name?: string;
  preferred_last_name?: string;
  profile?: Record<string, unknown>;
  member_offices?: { status?: string; state_id?: string; party?: string; office_type_id?: string; [field: string]: unknown }[];
  state?: string;
  party?: string;
  chamber?: string;
  [field: string]: unknown;
}

/** A staff row. */
export interface Staff {
  id?: number;
  staff_id?: number;
  first_name?: string;
  last_name?: string;
  preferred_first_name?: string;
  preferred_last_name?: string;
  title?: string;
  email?: string;
  positions?: Position[];
  staff_emails?: StaffEmail[];
  office_member_addresses?: OfficeMemberAddress[];
  [field: string]: unknown;
}

/** An office (committee, subcommittee, commission or administrative office). */
export interface Office {
  id?: number;
  name?: string;
  type?: string;
  chamber?: string;
  [field: string]: unknown;
}

/** A congressional caucus. */
export interface Caucus {
  id?: number;
  name?: string;
  [field: string]: unknown;
}

/** A town hall event. */
export interface TownHall {
  id?: number;
  date?: string;
  location?: string;
  member_name?: string;
  [field: string]: unknown;
}

/** A privately funded trip. */
export interface Trip {
  id?: number;
  destination?: string;
  sponsor?: string;
  traveler_name?: string;
  cost?: number | string;
  [field: string]: unknown;
}

/** A congressional hearing. */
export interface Hearing {
  id?: number;
  date?: string;
  title?: string;
  chamber?: string;
  committee?: string;
  [field: string]: unknown;
}

/** An issue assignment or caucus contact portfolio entry. */
export interface IssuePortfolio {
  name: string;
  kind: "issue" | "caucus_contact";
}

/** A staff row with its issue portfolios and convenience name lists. */
export interface StaffPortfolio extends Staff {
  issue_portfolios: IssuePortfolio[];
  issues: string[];
  caucus_contacts: string[];
}

/** A LegiStorm response envelope; list endpoints carry rows in `data`. */
export interface ListResponse<T> {
  data?: T[];
  results?: T[];
  items?: T[];
  [field: string]: unknown;
}

/** A retired/deleted ID list. */
export interface IdList {
  data?: number[];
  [field: string]: unknown;
}

/** A staff portfolio response; the status fields say what the issue lookup found. */
export interface StaffPortfoliosResponse {
  issue_endpoint: string | null;
  issue_portfolios_available: boolean;
  issue_portfolio_status: string;
  staff: StaffPortfolio[];
  [field: string]: unknown;
}

/** The unmodified body of an arbitrary endpoint, wrapped so the result is an object. */
export interface RawResponse {
  response: unknown;
}

interface ScopeQuery {
  kind: "member" | "office" | "staff";
  params: Params;
}

interface IssueMetadata {
  issue_endpoint: string | null;
  issue_portfolios_available: boolean;
  issue_portfolio_status: string;
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function rows(data: unknown): Record<string, any>[] {
  if (Array.isArray(data)) return data.filter(isRecord);
  if (isRecord(data)) {
    for (const key of ["data", "results", "items"]) {
      const value = data[key];
      if (Array.isArray(value)) return value.filter(isRecord);
    }
  }
  return [];
}

function isoDate(date: Date): string {
  return date.toISOString().slice(0, 10);
}

/** The CLI's default range: the last 30 days. */
function dateRange(args: { updated_from?: string; updated_to?: string }): { updated_from: string; updated_to: string } {
  if (args.updated_from && args.updated_to) {
    return { updated_from: args.updated_from, updated_to: args.updated_to };
  }
  const now = new Date();
  return {
    updated_from: args.updated_from ?? isoDate(new Date(now.getTime() - 30 * 86_400_000)),
    updated_to: args.updated_to ?? isoDate(now),
  };
}

async function request(ctx: Ctx, endpoint: string, params?: Params): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.LEGISTORM_API_KEY) headers["X-Api-Key"] = ctx.env.LEGISTORM_API_KEY;
  const res = await ctx.fetch((ctx.env.LEGISTORM_API_URL || DEFAULT_BASE) + endpoint + query(params ?? {}), { headers });
  return readJson(res, "LegiStorm");
}

function currentPositions(row: Record<string, any>): Record<string, any>[] {
  const positions = row.positions;
  return Array.isArray(positions) ? positions.filter((position) => isRecord(position) && position.is_current) : [];
}

function memberIds(row: Record<string, any>): number[] {
  const ids = new Set<number>();
  for (const position of currentPositions(row)) {
    const member = position.member;
    if (isRecord(member) && typeof member.member_id === "number") ids.add(member.member_id);
  }
  const addresses = Array.isArray(row.office_member_addresses) ? row.office_member_addresses : [];
  for (const address of addresses) {
    const member = isRecord(address) ? address.member : undefined;
    if (isRecord(member) && typeof member.member_id === "number") ids.add(member.member_id);
  }
  return [...ids].sort((a, b) => a - b);
}

function officeIds(row: Record<string, any>): number[] {
  const ids = new Set<number>();
  for (const position of currentPositions(row)) {
    const office = position.office;
    if (isRecord(office) && typeof office.office_id === "number") ids.add(office.office_id);
  }
  const addresses = Array.isArray(row.office_member_addresses) ? row.office_member_addresses : [];
  for (const address of addresses) {
    const office = isRecord(address) ? address.office : undefined;
    if (isRecord(office) && typeof office.office_id === "number") ids.add(office.office_id);
  }
  return [...ids].sort((a, b) => a - b);
}

function staffId(row: Record<string, any>): number | undefined {
  const candidates: unknown[] = [
    row.staff_id,
    row.staffer_id,
    row.person_id,
    row.contact_id,
    row.id,
    isRecord(row.staff) ? row.staff.id : undefined,
    isRecord(row.contact) ? row.contact.id : undefined,
  ];
  for (const candidate of candidates) {
    if (typeof candidate === "number" && Number.isInteger(candidate)) return candidate;
    if (typeof candidate === "string" && /^\d+$/.test(candidate)) return Number(candidate);
  }
  return undefined;
}

function firstString(...values: unknown[]): string | undefined {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return undefined;
}

function issueName(row: Record<string, any>): string | undefined {
  if (isRecord(row.issue)) {
    return firstString(row.issue.name, row.issue.issue_name, row.issue.display_name, row.issue.title);
  }
  return firstString(row.issue_name, row.issue, row.legislative_issue, row.issue_title, row.name, row.title, row.subject);
}

function portfolioKind(row: Record<string, any>): "issue" | "caucus_contact" {
  return Object.keys(row).some((key) => key.toLowerCase().includes("caucus")) ? "caucus_contact" : "issue";
}

function normaliseIssuePortfolios(data: unknown): Record<number, IssuePortfolio[]> {
  const byStaff: Record<number, IssuePortfolio[]> = {};
  const add = (id: number | undefined, name: string | undefined, kind: "issue" | "caucus_contact"): void => {
    if (!id || !name) return;
    const portfolios = (byStaff[id] ??= []);
    if (!portfolios.some((item) => item.name === name && item.kind === kind)) portfolios.push({ name, kind });
  };
  for (const row of rows(data)) {
    const id = staffId(row);
    let embeddedSeen = false;
    for (const key of ["issues", "legislative_issues", "issue_portfolios", "issue_assignments"]) {
      const embedded = row[key];
      if (!Array.isArray(embedded)) continue;
      embeddedSeen = true;
      for (const item of embedded) {
        if (isRecord(item)) add(id, issueName(item), portfolioKind(item));
        else if (typeof item === "string") add(id, item, "issue");
      }
    }
    if (!embeddedSeen) add(id, issueName(row), portfolioKind(row));
  }
  return byStaff;
}

function normaliseEndpoint(endpoint: string): string {
  return endpoint.startsWith("/") ? endpoint : `/${endpoint}`;
}

function candidateIssueEndpoints(ctx: Ctx, explicit?: string): string[] {
  const configured = explicit || ctx.env.LEGISTORM_ISSUES_ENDPOINT || "";
  const candidates: string[] = [];
  if (configured) candidates.push(normaliseEndpoint(configured));
  for (const candidate of ISSUE_ENDPOINT_CANDIDATES) {
    if (!candidates.includes(candidate)) candidates.push(candidate);
  }
  return candidates;
}

function issueScopeQueries(
  staffRows: Record<string, any>[],
  range: { updated_from: string; updated_to: string },
  limit: number,
  page: number,
): ScopeQuery[] {
  const members = new Set<number>();
  const offices = new Set<number>();
  const staff = new Set<number>();
  for (const row of staffRows) {
    for (const id of memberIds(row)) members.add(id);
    for (const id of officeIds(row)) offices.add(id);
    const id = staffId(row);
    if (id) staff.add(id);
  }
  const scopedLimit = Math.min(limit, 1000);
  const queries: ScopeQuery[] = [];
  for (const member_id of [...members].sort((a, b) => a - b)) {
    queries.push({
      kind: "member",
      params: { ...range, limit: scopedLimit, page, member_id },
    });
  }
  for (const office_id of [...offices].sort((a, b) => a - b)) {
    queries.push({
      kind: "office",
      params: { ...range, limit: scopedLimit, page, office_id },
    });
  }
  for (const staff_id of [...staff].sort((a, b) => a - b)) {
    queries.push({
      kind: "staff",
      params: { ...range, limit: scopedLimit, page, staff_id },
    });
  }
  return queries;
}

async function fetchIssuePortfolios(
  ctx: Ctx,
  staffRows: Record<string, any>[],
  range: { updated_from: string; updated_to: string },
  limit: number,
  page: number,
  issueEndpoint?: string,
): Promise<{ portfolios: Record<number, IssuePortfolio[]>; metadata: IssueMetadata }> {
  const queries = issueScopeQueries(staffRows, range, limit, page);
  if (queries.length === 0) {
    return {
      portfolios: {},
      metadata: {
        issue_endpoint: null,
        issue_portfolios_available: false,
        issue_portfolio_status: "no_supported_scope",
      },
    };
  }
  for (const endpoint of candidateIssueEndpoints(ctx, issueEndpoint)) {
    for (const scope of queries) {
      let data: unknown;
      try {
        data = await request(ctx, endpoint, scope.params);
      } catch {
        continue;
      }
      const portfolios = normaliseIssuePortfolios(data);
      if (Object.values(portfolios).some((items) => items.length > 0)) {
        return {
          portfolios,
          metadata: {
            issue_endpoint: endpoint,
            issue_portfolios_available: true,
            issue_portfolio_status: `ok:${scope.kind}`,
          },
        };
      }
    }
  }
  return {
    portfolios: {},
    metadata: {
      issue_endpoint: issueEndpoint ? normaliseEndpoint(issueEndpoint) : null,
      issue_portfolios_available: false,
      issue_portfolio_status: "no_issue_endpoint_discovered",
    },
  };
}

function withPortfolios(
  staffRows: Record<string, any>[],
  portfolios: Record<number, IssuePortfolio[]>,
): StaffPortfolio[] {
  return staffRows.map((row) => {
    const id = staffId(row);
    const all = (id !== undefined ? portfolios[id] : undefined) ?? [];
    return {
      ...row,
      issue_portfolios: all,
      issues: all.filter((item) => item.kind === "issue").map((item) => item.name),
      caucus_contacts: all.filter((item) => item.kind === "caucus_contact").map((item) => item.name),
    };
  });
}

/**
 * Congressional members, with profile, current offices, state, party and chamber. `updated_from` and
 * `updated_to` are YYYY-MM-DD dates (default: the last 30 days). `status` is a=all, c=current,
 * i=incoming, d=departing; `state_id` is a postal abbreviation. `limit` is at most 1000; page
 * through with `page` (1-based).
 */
export async function get_members(
  args: {
    updated_from?: string;
    updated_to?: string;
    limit?: number;
    page?: number;
    member_id?: number;
    state_id?: string;
    status?: string;
  },
  ctx: Ctx,
): Promise<ListResponse<Member>> {
  const range = dateRange(args);
  const params: Params = {
    ...range,
    limit: Math.min(args.limit ?? 20, 1000),
    page: args.page ?? 1,
    status: args.status ?? "a",
  };
  if (args.member_id) params.id = args.member_id;
  if (args.state_id) params.state_id = args.state_id;
  return request(ctx, "/member/list", params);
}

/**
 * Congressional staff, with current positions, office and member references and emails.
 * `updated_from`/`updated_to` are YYYY-MM-DD (default: the last 30 days). Filter by `staff_id`,
 * `member_id`, or `office_id`. `limit` is at most 1000; page through with `page` (1-based).
 */
export async function get_staff(
  args: {
    updated_from?: string;
    updated_to?: string;
    limit?: number;
    page?: number;
    staff_id?: number;
    member_id?: number;
    office_id?: number;
  },
  ctx: Ctx,
): Promise<ListResponse<Staff>> {
  const range = dateRange(args);
  const params: Params = {
    ...range,
    limit: Math.min(args.limit ?? 20, 1000),
    page: args.page ?? 1,
  };
  if (args.staff_id) params.id = args.staff_id;
  if (args.member_id) params.member_id = args.member_id;
  if (args.office_id) params.office_id = args.office_id;
  return request(ctx, "/staff/list", params);
}

/**
 * Staff rows plus their legislative issue portfolios and caucus contacts. LegiStorm only exposes
 * issues on some keys, so this probes a candidate issue endpoint (an explicit `issue_endpoint`, then
 * `LEGISTORM_ISSUES_ENDPOINT`, then known routes) with member, office and staff scopes and may make
 * several requests. Status says whether an endpoint worked (`issue_portfolios_available`); rows still
 * come back with empty portfolios when none did. Dates default to the last 30 days; same filters and
 * paging as `get_staff`.
 */
export async function get_staff_with_issue_portfolios(
  args: {
    updated_from?: string;
    updated_to?: string;
    limit?: number;
    page?: number;
    staff_id?: number;
    member_id?: number;
    office_id?: number;
    issue_endpoint?: string;
  },
  ctx: Ctx,
): Promise<StaffPortfoliosResponse> {
  const range = dateRange(args);
  const staff = await get_staff(
    {
      ...range,
      limit: args.limit,
      page: args.page,
      staff_id: args.staff_id,
      member_id: args.member_id,
      office_id: args.office_id,
    },
    ctx,
  );
  const staffRows = rows(staff);
  const { portfolios, metadata } = await fetchIssuePortfolios(
    ctx,
    staffRows,
    range,
    args.limit ?? 20,
    args.page ?? 1,
    args.issue_endpoint,
  );
  return { ...metadata, staff: withPortfolios(staffRows, portfolios) };
}

/** IDs of staff no longer employed by Congress. Takes no arguments. */
export async function get_staff_retired_ids(args: Record<string, never>, ctx: Ctx): Promise<IdList> {
  return request(ctx, "/staff/retired-ids");
}

/**
 * Offices such as committees, subcommittees, commissions and administrative offices. Dates are
 * YYYY-MM-DD (default: the last 30 days); `office_id` fetches one office. `limit` is at most 1000.
 */
export async function get_offices(
  args: { updated_from?: string; updated_to?: string; limit?: number; page?: number; office_id?: number },
  ctx: Ctx,
): Promise<ListResponse<Office>> {
  const range = dateRange(args);
  const params: Params = { ...range, limit: Math.min(args.limit ?? 20, 1000), page: args.page ?? 1 };
  if (args.office_id) params.id = args.office_id;
  return request(ctx, "/office/list", params);
}

/** IDs of inactive offices. Takes no arguments. */
export async function get_offices_retired_ids(args: Record<string, never>, ctx: Ctx): Promise<IdList> {
  return request(ctx, "/office/retired-ids");
}

/**
 * Congressional caucuses (requires a caucus subscription). Dates are YYYY-MM-DD (default: the last
 * 30 days); `caucus_id` fetches one caucus. `limit` is at most 1000.
 */
export async function get_caucuses(
  args: { updated_from?: string; updated_to?: string; limit?: number; page?: number; caucus_id?: number },
  ctx: Ctx,
): Promise<ListResponse<Caucus>> {
  const range = dateRange(args);
  const params: Params = { ...range, limit: Math.min(args.limit ?? 20, 1000), page: args.page ?? 1 };
  if (args.caucus_id) params.id = args.caucus_id;
  return request(ctx, "/caucus/list", params);
}

/** IDs of inactive or deleted caucuses. Takes no arguments. */
export async function get_caucuses_retired_ids(args: Record<string, never>, ctx: Ctx): Promise<IdList> {
  return request(ctx, "/caucus/retired-ids");
}

/**
 * Town hall events, with date, location and member name. Dates are YYYY-MM-DD (default: the last 30
 * days); `townhall_id` fetches one event. `limit` is at most 100.
 */
export async function get_townhalls(
  args: { updated_from?: string; updated_to?: string; limit?: number; page?: number; townhall_id?: number },
  ctx: Ctx,
): Promise<ListResponse<TownHall>> {
  const range = dateRange(args);
  const params: Params = { ...range, limit: Math.min(args.limit ?? 20, 100), page: args.page ?? 1 };
  if (args.townhall_id) params.id = args.townhall_id;
  return request(ctx, "/townhall/list", params);
}

/**
 * Privately funded congressional travel, with destination, sponsor, traveler and cost. Dates are
 * YYYY-MM-DD (default: the last 30 days); `trip_id` fetches one trip. `limit` is at most 100.
 */
export async function get_trips(
  args: { updated_from?: string; updated_to?: string; limit?: number; page?: number; trip_id?: number },
  ctx: Ctx,
): Promise<ListResponse<Trip>> {
  const range = dateRange(args);
  const params: Params = { ...range, limit: Math.min(args.limit ?? 20, 100), page: args.page ?? 1 };
  if (args.trip_id) params.id = args.trip_id;
  return request(ctx, "/trips/list", params);
}

/**
 * Congressional hearings, with date, title, chamber and committee. `chamber` is H=House or S=Senate.
 * Updated dates and `hearing_date_from`/`hearing_date_to` are YYYY-MM-DD (updated range defaults to
 * the last 30 days). `office_id` filters by committee; `hearing_id` fetches one hearing. `limit` is at
 * most 100.
 */
export async function get_hearings(
  args: {
    updated_from?: string;
    updated_to?: string;
    chamber?: string;
    limit?: number;
    page?: number;
    hearing_id?: number;
    office_id?: number;
    hearing_date_from?: string;
    hearing_date_to?: string;
  },
  ctx: Ctx,
): Promise<ListResponse<Hearing>> {
  const range = dateRange(args);
  const params: Params = {
    ...range,
    chamber: args.chamber ?? "H",
    limit: Math.min(args.limit ?? 20, 100),
    page: args.page ?? 1,
  };
  if (args.hearing_id) params.id = args.hearing_id;
  if (args.office_id) params.office_id = args.office_id;
  if (args.hearing_date_from) params.hearing_date_from = args.hearing_date_from;
  if (args.hearing_date_to) params.hearing_date_to = args.hearing_date_to;
  return request(ctx, "/hearings/list", params);
}

/**
 * GET any LegiStorm Congress endpoint, such as `/member/list` or `/staff/retired-ids`, with your own
 * query parameters. The parsed body is returned unchanged under `response` (which may itself be an
 * array or an envelope).
 */
export async function raw_request(
  args: { endpoint: string; params?: Record<string, string | number | boolean> },
  ctx: Ctx,
): Promise<RawResponse> {
  const endpoint = normaliseEndpoint(args.endpoint);
  return { response: await request(ctx, endpoint, args.params) };
}
