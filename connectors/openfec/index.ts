import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Responses are OpenFEC's own envelopes: `api_version`, `pagination` and
// `results`. Interfaces name the fields worth knowing; the API sends more.

const DEFAULT_BASE = "https://api.open.fec.gov/v1";

/** Paging metadata sent with every list response. */
export interface Pagination {
  count?: number;
  is_count_exact?: boolean;
  page?: number;
  pages?: number;
  per_page?: number;
  [field: string]: unknown;
}

/** An OpenFEC list response. */
export interface ListResponse<T> {
  api_version?: string;
  pagination?: Pagination;
  results: T[];
  [field: string]: unknown;
}

/** A candidate. */
export interface Candidate {
  candidate_id?: string;
  name?: string;
  party?: string;
  party_full?: string;
  office?: string;
  office_full?: string;
  state?: string;
  district?: string;
  election_year?: number;
  election_years?: number[];
  cycles?: number[];
  candidate_status?: string;
  incumbent_challenge?: string | null;
  has_raised_funds?: boolean;
  principal_committees?: Committee[];
  [field: string]: unknown;
}

/** A committee. */
export interface Committee {
  committee_id?: string;
  name?: string;
  committee_type?: string;
  committee_type_full?: string;
  designation?: string;
  designation_full?: string;
  party?: string;
  state?: string;
  cycles?: number[];
  treasurer_name?: string;
  filing_frequency?: string;
  first_file_date?: string;
  last_file_date?: string;
  [field: string]: unknown;
}

/** One itemized contribution (Schedule A row). Amounts are USD. */
export interface Contribution {
  committee_id?: string;
  committee?: { committee_id?: string; name?: string; [field: string]: unknown };
  contributor_id?: string;
  contributor_name?: string;
  contributor_first_name?: string;
  contributor_last_name?: string;
  contributor_city?: string;
  contributor_state?: string;
  contributor_zip?: string;
  contribution_receipt_amount?: number;
  contribution_receipt_date?: string;
  entity_type?: string;
  entity_type_desc?: string;
  memo_text?: string;
  sub_id?: number;
  amendment_indicator?: string;
  [field: string]: unknown;
}

/** A committee filing report. */
export interface Filing {
  committee_id?: string;
  committee_name?: string;
  form_type?: string;
  filing_date?: string;
  receipt_date?: string;
  report_type?: string;
  report_year?: number;
  cycle?: number;
  document_type?: string;
  beginning_date?: string;
  ending_date?: string;
  total_receipts?: number;
  total_disbursements?: number;
  is_amended?: boolean;
  amendment_indicator?: string;
  fec_file_id?: string;
  pdf_url?: string;
  [field: string]: unknown;
}

/** A candidate's financial totals for one cycle. Amounts are USD. */
export interface CandidateTotals {
  candidate_id?: string;
  cycle?: number;
  election_year?: number;
  office?: string;
  party?: string;
  state?: string;
  district?: string;
  total_receipts?: number;
  receipts?: number;
  total_disbursements?: number;
  disbursements?: number;
  cash_on_hand_end_period?: number;
  last_cash_on_hand_end_period?: number;
  debts_owed_by_committee?: number;
  last_debts_owed_by_committee?: number;
  contributions?: number;
  individual_contributions?: number;
  coverage_start_date?: string;
  coverage_end_date?: string;
  [field: string]: unknown;
}

async function request(ctx: Ctx, path: string, params: Params): Promise<any> {
  const all: Params = { ...params };
  // Behind iron-proxy the key is injected as a query parameter on the wire;
  // locally it comes from the env.
  if (ctx.env.DATAGOV_API_KEY) all.api_key = ctx.env.DATAGOV_API_KEY;
  const res = await ctx.fetch((ctx.env.OPENFEC_API_URL || DEFAULT_BASE) + path + query(all), {
    headers: { accept: "application/json" },
  });
  return readJson(res, "OpenFEC");
}

/**
 * Search candidates by name, two-letter `state`, party code (`DEM`, `REP`, ...), office
 * (H=House, S=Senate, P=President) and two-year `cycle`. `per_page` is 1-100 (default 20); page
 * through with `page` (1-based) and read `pagination`.
 */
export async function search_candidates(
  args: {
    name?: string;
    state?: string;
    party?: string;
    office?: string;
    cycle?: number;
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<ListResponse<Candidate>> {
  const params: Params = { per_page: args.per_page ?? 20, page: args.page ?? 1 };
  if (args.name) params.name = args.name;
  if (args.state) params.state = args.state;
  if (args.party) params.party = args.party;
  if (args.office) params.office = args.office;
  if (args.cycle) params.cycle = args.cycle;
  return request(ctx, "/candidates/search/", params);
}

/**
 * One candidate by FEC candidate ID (for example H0CA12183), including principal committees. The
 * candidate is in `results` (usually one entry).
 */
export async function get_candidate(args: { candidate_id: string }, ctx: Ctx): Promise<ListResponse<Candidate>> {
  return request(ctx, `/candidate/${encodeURIComponent(args.candidate_id)}/`, {});
}

/**
 * Search committees by name (`name` becomes the API's `q`), two-letter `state`, committee type code
 * and two-year `cycle`. `per_page` is 1-100 (default 20); page through with `page`.
 */
export async function search_committees(
  args: {
    name?: string;
    state?: string;
    committee_type?: string;
    cycle?: number;
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<ListResponse<Committee>> {
  const params: Params = { per_page: args.per_page ?? 20, page: args.page ?? 1 };
  if (args.name) params.q = args.name;
  if (args.state) params.state = args.state;
  if (args.committee_type) params.committee_type = args.committee_type;
  if (args.cycle) params.cycle = args.cycle;
  return request(ctx, "/committees/", params);
}

/**
 * Itemized contributions (Schedule A): who gave what to which committee, when, and where the
 * contributor lives. Filter by `committee_id`, contributor name or two-letter `contributor_state`,
 * USD `min_amount`/`max_amount` and YYYY-MM-DD `min_date`/`max_date` receipt dates. `per_page` is
 * 1-100 (default 20); page through with `page`.
 */
export async function get_contributions(
  args: {
    committee_id?: string;
    contributor_name?: string;
    contributor_state?: string;
    min_amount?: number;
    max_amount?: number;
    min_date?: string;
    max_date?: string;
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<ListResponse<Contribution>> {
  const params: Params = { per_page: args.per_page ?? 20, page: args.page ?? 1 };
  if (args.committee_id) params.committee_id = args.committee_id;
  if (args.contributor_name) params.contributor_name = args.contributor_name;
  if (args.contributor_state) params.contributor_state = args.contributor_state;
  if (args.min_amount !== undefined) params.min_amount = args.min_amount;
  if (args.max_amount !== undefined) params.max_amount = args.max_amount;
  if (args.min_date) params.min_date = args.min_date;
  if (args.max_date) params.max_date = args.max_date;
  return request(ctx, "/schedules/schedule_a/", params);
}

/**
 * Filings a committee submitted (form type, report type, period, receipt/disbursement totals, PDF
 * URL). `committee_id` is required; `form_type` filters by form (such as F3, F3N or F24).
 * `per_page` is 1-100 (default 20); page through with `page`.
 */
export async function get_filings(
  args: { committee_id: string; form_type?: string; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<ListResponse<Filing>> {
  const params: Params = { committee_id: args.committee_id, per_page: args.per_page ?? 20, page: args.page ?? 1 };
  if (args.form_type) params.form_type = args.form_type;
  return request(ctx, "/filings/", params);
}

/**
 * A candidate's financial totals by FEC candidate ID and optional two-year `cycle`: receipts
 * (`receipts`), disbursements (`disbursements`), cash on hand and debts, contribution breakdowns and
 * coverage dates. Amounts are USD. Results are in `results`, usually one row per cycle.
 */
export async function get_candidate_totals(
  args: { candidate_id: string; cycle?: number },
  ctx: Ctx,
): Promise<ListResponse<CandidateTotals>> {
  const params: Params = {};
  if (args.cycle) params.cycle = args.cycle;
  return request(ctx, `/candidate/${encodeURIComponent(args.candidate_id)}/totals/`, params);
}
