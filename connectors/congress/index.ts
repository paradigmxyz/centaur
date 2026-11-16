import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are the Congress.gov API's own objects, passed through; lists keep the
// API's `pagination` envelope (the `/house-vote` envelope's `houseRollCallVotes`
// becomes `votes`). Field names are Congress.gov's
// (https://api.congress.gov/); the interfaces list the fields worth knowing.

const DEFAULT_BASE = "https://api.congress.gov/v3";

/** A page of results. `count` is the total number of records for the query. */
export interface Pagination {
  count: number;
  /** URL of the next page, absent on the last page. */
  next?: string;
  [field: string]: unknown;
}

export interface LatestAction {
  actionDate?: string;
  actionTime?: string;
  text?: string;
  [field: string]: unknown;
}

/** A sponsor or cosponsor of a bill or amendment. */
export interface Sponsor {
  bioguideId?: string;
  /** The cosponsors endpoint spells the field `bioguidId`. */
  bioguidId?: string;
  firstName?: string;
  middleName?: string;
  lastName?: string;
  fullName?: string;
  party?: string;
  state?: string;
  district?: number;
  isByRequest?: string;
  url?: string;
  [field: string]: unknown;
}

export interface Cosponsor extends Sponsor {
  sponsorshipDate?: string;
  isOriginalCosponsor?: boolean;
  sponsorshipWithdrawnDate?: string;
}

export interface Bill {
  congress: number;
  /** hr, s, hjres, sjres, hconres, sconres, hres, sres. */
  type: string;
  number: number | string;
  originChamber?: string;
  originChamberCode?: string;
  title?: string;
  introducedDate?: string;
  latestAction?: LatestAction;
  policyArea?: { name?: string; [field: string]: unknown };
  sponsors?: Sponsor[];
  legislationUrl?: string;
  relatedBills?: string;
  laws?: { number?: number | string; type?: string; [field: string]: unknown }[];
  actions?: { count?: number; url?: string; [field: string]: unknown };
  committees?: { count?: number; url?: string; [field: string]: unknown };
  updateDate?: string;
  updateDateIncludingText?: string;
  url?: string;
  [field: string]: unknown;
}

export interface BillAction {
  actionDate?: string;
  actionTime?: string;
  actionCode?: string | number;
  text?: string;
  type?: string;
  sourceSystem?: { name?: string; code?: number | string; [field: string]: unknown };
  [field: string]: unknown;
}

export interface Amendment {
  congress?: number;
  number?: number | string;
  type?: string;
  description?: string;
  purpose?: string;
  latestAction?: LatestAction;
  updateDate?: string;
  url?: string;
  [field: string]: unknown;
}

export interface LegislativeSubject {
  name?: string;
  updateDate?: string;
  [field: string]: unknown;
}

export interface Summary {
  versionCode?: string;
  actionDate?: string;
  actionDesc?: string;
  text?: string;
  updateDate?: string;
  [field: string]: unknown;
}

export interface TextVersion {
  date?: string;
  type?: string;
  formats?: { type?: string; url?: string; isErrata?: boolean | string }[];
  [field: string]: unknown;
}

export interface BillTitle {
  title?: string;
  titleType?: string;
  titleTypeCode?: number | string;
  billTextVersionCode?: string;
  [field: string]: unknown;
}

export interface RelatedBill {
  congress?: number;
  number?: number | string;
  type?: string;
  title?: string;
  latestAction?: LatestAction;
  relationshipDetails?: { type?: string; identifiedBy?: string; [field: string]: unknown }[];
  [field: string]: unknown;
}

export interface BillDetail {
  actions?: BillAction[];
  amendments?: Amendment[];
  cosponsors?: Cosponsor[];
  relatedBills?: RelatedBill[];
  subjects?: {
    legislativeSubjects?: LegislativeSubject[];
    policyArea?: { name?: string; [field: string]: unknown };
    [field: string]: unknown;
  };
  summaries?: Summary[];
  textVersions?: TextVersion[];
  titles?: BillTitle[];
  committees?: Committee[];
  pagination?: Pagination;
  [resource: string]: unknown;
}

export interface PartyHistory {
  partyAbbreviation?: string;
  partyName?: string;
  startYear?: number;
  endYear?: number;
  [field: string]: unknown;
}

export interface MemberTerm {
  chamber?: string;
  congress?: number;
  startYear?: number;
  endYear?: number;
  memberType?: string;
  stateCode?: string;
  stateName?: string;
  [field: string]: unknown;
}

export interface Member {
  bioguideId: string;
  /** Inverted display name, such as "Leahy, Patrick J.". */
  name?: string;
  partyName?: string;
  party?: string;
  state?: string;
  district?: number;
  firstName?: string;
  lastName?: string;
  directOrderName?: string;
  invertedOrderName?: string;
  honorificName?: string;
  birthYear?: string | number;
  currentMember?: boolean;
  depiction?: { imageUrl?: string; attribution?: string; [field: string]: unknown };
  officialWebsiteUrl?: string;
  /** Seat history: the list endpoints nest it as `{ item: [...] }`, the detail endpoint sends an array. */
  terms?: MemberTerm[] | { item?: MemberTerm[] };
  partyHistory?: PartyHistory[];
  addressInformation?: {
    officeAddress?: string;
    city?: string;
    district?: string;
    zipCode?: number | string;
    phoneNumber?: string;
    [field: string]: unknown;
  };
  sponsoredLegislation?: { count?: number; url?: string; [field: string]: unknown };
  cosponsoredLegislation?: { count?: number; url?: string; [field: string]: unknown };
  updateDate?: string;
  url?: string;
  [field: string]: unknown;
}

export interface Committee {
  systemCode?: string;
  name?: string;
  chamber?: string;
  committeeTypeCode?: string;
  type?: string;
  isCurrent?: boolean;
  committeeWebsiteUrl?: string;
  parent?: { systemCode?: string; name?: string; [field: string]: unknown };
  subcommittees?: { systemCode?: string; name?: string; [field: string]: unknown }[];
  activities?: { date?: string; name?: string; [field: string]: unknown }[];
  history?: unknown[];
  updateDate?: string;
  url?: string;
  [field: string]: unknown;
}

export interface Hearing {
  congress?: number;
  chamber?: string;
  number?: number | string;
  part?: number | string;
  /** Only on the hearing detail; the list endpoints omit the title. */
  title?: string;
  /** Only on the hearing detail. */
  dates?: string[];
  /** Only on the hearing detail. */
  citation?: string;
  jacketNumber?: number | string;
  libraryOfCongressidentifier?: string;
  formats?: { type?: string; url?: string; [field: string]: unknown }[];
  committees?: { systemCode?: string; name?: string; [field: string]: unknown }[];
  updateDate?: string;
  url?: string;
  [field: string]: unknown;
}

/** One House roll call vote. Congress.gov does not publish Senate roll call votes. */
export interface HouseVote {
  congress?: number;
  identifier?: number;
  sessionNumber?: number;
  rollCallNumber?: number;
  legislationNumber?: string;
  legislationType?: string;
  legislationUrl?: string;
  result?: string;
  voteType?: string;
  /** Asked only by the vote detail endpoint. */
  voteQuestion?: string;
  sourceDataURL?: string;
  startDate?: string;
  updateDate?: string;
  url?: string;
  [field: string]: unknown;
}

async function request(ctx: Ctx, path: string, params: Params = {}): Promise<any> {
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  const url =
    (ctx.env.CONGRESS_API_URL || DEFAULT_BASE) +
    path +
    query({ ...params, api_key: ctx.env.DATAGOV_API_KEY, format: "json" });
  return readJson(await ctx.fetch(url, { headers: { accept: "application/json" } }), "Congress.gov");
}

function segment(value: string | number): string {
  return encodeURIComponent(String(value));
}

/**
 * Bills of one Congress, as the API orders them. `bill_type` restricts to one type: hr, s, hjres,
 * sjres, hconres, sconres, hres or sres. `limit` is at most 250; page with `offset` (0 is the
 * first record) or follow `pagination.next`.
 */
export async function list_bills(
  args: { congress?: number; bill_type?: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<{ bills: Bill[]; pagination: Pagination }> {
  const congress = args.congress ?? 119;
  const path = args.bill_type
    ? `/bill/${congress}/${segment(args.bill_type)}`
    : `/bill/${congress}`;
  return request(ctx, path, { limit: args.limit, offset: args.offset });
}

/** One bill: title, origin chamber, introduced date, latest action, policy area, sponsors, laws. */
export async function get_bill(
  args: { congress: number; bill_type: string; number: number },
  ctx: Ctx,
): Promise<{ bill: Bill }> {
  return request(ctx, `/bill/${args.congress}/${segment(args.bill_type)}/${args.number}`);
}

/**
 * One bill sub-resource, as the API returns it. `detail` is actions, amendments, cosponsors,
 * relatedbills, subjects, summaries, text, titles or committees; the result holds that resource's
 * key (`actions`, `amendments`, `cosponsors`, `relatedBills`, `subjects`, `summaries`,
 * `textVersions`, `titles`, `committees`) plus `pagination`. `text` gives the text-version links.
 */
export async function bill_detail(
  args: {
    congress: number;
    bill_type: string;
    number: number;
    detail:
      | "actions" | "amendments" | "cosponsors" | "relatedbills" | "subjects" | "summaries"
      | "text" | "titles" | "committees";
    limit?: number;
    offset?: number;
  },
  ctx: Ctx,
): Promise<BillDetail> {
  return request(
    ctx,
    `/bill/${args.congress}/${segment(args.bill_type)}/${args.number}/${segment(args.detail)}`,
    { limit: args.limit, offset: args.offset },
  );
}

/**
 * Members of one Congress, with bioguide ID, name, party, state/district and photo. `limit` is at
 * most 250; page with `offset`. With `state` (a postal code such as CA or NY) this lists the
 * state's current members instead, optionally `current_member: false` for former ones; that
 * endpoint filters by state only, not by Congress, and has no `offset`.
 */
export async function list_members(
  args: {
    congress?: number;
    state?: string;
    current_member?: boolean;
    limit?: number;
    offset?: number;
  },
  ctx: Ctx,
): Promise<{ members: Member[]; pagination: Pagination }> {
  const congress = args.congress ?? 119;
  if (!args.state) {
    return request(ctx, `/member/congress/${congress}`, { limit: args.limit, offset: args.offset });
  }
  const current = args.current_member === false ? undefined : true;
  return request(ctx, `/member/${segment(args.state.toUpperCase())}`, {
    currentMember: current,
    limit: args.limit,
  });
}

/** One member by bioguide ID (for example L000174): biography, party history, terms, website. */
export async function get_member(
  args: { bioguide_id: string },
  ctx: Ctx,
): Promise<{ member: Member }> {
  return request(ctx, `/member/${segment(args.bioguide_id)}`);
}

/**
 * Committees of one Congress. `chamber` is house, senate or joint; omit it for all chambers.
 * `limit` is at most 250; page with `offset`.
 */
export async function list_committees(
  args: { congress?: number; chamber?: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<{ committees: Committee[]; pagination: Pagination }> {
  const congress = args.congress ?? 119;
  const path = args.chamber
    ? `/committee/${congress}/${segment(args.chamber)}`
    : `/committee/${congress}`;
  return request(ctx, path, { limit: args.limit, offset: args.offset });
}

/**
 * Hearings of one Congress: chamber, congress, jacket number, item number and update date. The list
 * omits titles and dates; call `get_hearing` with the listed `chamber` and `jacket_number` for
 * those. `chamber` is house or senate; omit it for both. `limit` is at most 250; page with
 * `offset`.
 */
export async function list_hearings(
  args: { congress?: number; chamber?: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<{ hearings: Hearing[]; pagination: Pagination }> {
  const congress = args.congress ?? 119;
  const path = args.chamber
    ? `/hearing/${congress}/${segment(args.chamber)}`
    : `/hearing/${congress}`;
  return request(ctx, path, { limit: args.limit, offset: args.offset });
}

/**
 * One hearing by chamber and jacket number (both from `list_hearings`): title, dates, citation,
 * committees and the bound-hearing formats and URLs.
 */
export async function get_hearing(
  args: { congress: number; chamber: string; jacket_number: number | string },
  ctx: Ctx,
): Promise<{ hearing: Hearing }> {
  return request(
    ctx,
    `/hearing/${args.congress}/${segment(args.chamber)}/${segment(args.jacket_number)}`,
  );
}

/**
 * House roll call votes of one Congress: roll call number, session, date, result, vote type and the
 * bill voted on. Congress.gov v3 publishes only House roll call votes, so `chamber` may only be
 * "house"; `session` 1 or 2 narrows to one session. `limit` is at most 250; page with `offset`.
 */
export async function list_votes(
  args: {
    congress?: number;
    chamber?: string;
    session?: number;
    limit?: number;
    offset?: number;
  },
  ctx: Ctx,
): Promise<{ votes: HouseVote[]; pagination: Pagination }> {
  if (args.chamber && args.chamber.toLowerCase() !== "house") {
    throw new Error(
      `Congress.gov publishes only House roll call votes; got chamber "${args.chamber}"`,
    );
  }
  const congress = args.congress ?? 119;
  const path = args.session
    ? `/house-vote/${congress}/${args.session}`
    : `/house-vote/${congress}`;
  const data = await request(ctx, path, { limit: args.limit, offset: args.offset });
  return { votes: data.houseRollCallVotes ?? [], pagination: data.pagination };
}
