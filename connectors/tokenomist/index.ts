import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are Tokenomist's own objects, passed through: field names and types are
// theirs. Responses carry `metadata` (query time and the calling account's credit
// usage) next to `data`. The API also reports failures in a 200 body
// (`status: false` plus `errorMessage`), which is thrown like an HTTP error.
// Token ids come from `list_tokens`.

const DEFAULT_BASE = "https://api.tokenomist.ai";

/** The query time and the calling account's credit usage for this response. */
export interface Metadata {
  queryDate?: string;
  credit?: { used?: number; limit?: number; resetAt?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** A token, with supply, lock and unlock totals (amounts are in tokens; values in USD). */
export interface Token {
  id: string;
  name: string;
  symbol: string;
  circulatingSupply?: number | null;
  marketCap?: number | null;
  maxSupply?: number | null;
  totalLockedAmount?: number;
  tbdLockedAmount?: number;
  unlockedAmount?: number;
  untrackedAmount?: number;
  hasStandardAllocation?: boolean;
  websiteUrl?: string;
  lastUpdatedDate?: string;
  [field: string]: unknown;
}

/** One row of the allocation table: scheduled or still TBD. */
export interface Allocation {
  allocationName: string;
  allocationType: "Scheduled" | "TBD" | string;
  standardAllocationName?: string;
  allocationUnlockedAmount?: number;
  allocationLockedAmount?: number;
  allocationAmount?: number;
  trackedAllocationPercentage?: number;
  [field: string]: unknown;
}

/** A token's allocation table and its supply totals. */
export interface Allocations {
  name?: string;
  symbol?: string;
  maxSupply?: number | null;
  lastUpdatedDate?: string;
  totalUnlockedAmount?: number;
  totalLockedAmount?: number;
  totalUntrackedAmount?: number;
  totalTBDLockedAmount?: number;
  allocations?: Allocation[];
  [field: string]: unknown;
}

/** Part of an unlock event, broken down by allocation. */
export interface AllocationBreakdown {
  allocationName?: string;
  standardAllocationName?: string;
  unlockAmount?: number;
  unlockValue?: number;
  [field: string]: unknown;
}

/** A scheduled cliff unlock event. */
export interface CliffUnlocks {
  cliffAmount?: number;
  cliffValue?: number;
  valueToMarketCap?: number;
  allocationBreakdown?: AllocationBreakdown[];
  [field: string]: unknown;
}

/** A linear unlock stream that started at a given event. */
export interface LinearUnlocks {
  linearAmount?: number;
  linearValue?: number;
  dailyAmount?: number;
  dailyValue?: number;
  referencePrice?: number;
  referencePriceUpdatedTime?: string;
  allocationBreakdown?: AllocationBreakdown[];
  [field: string]: unknown;
}

/** One unlock event (`unlockDate` is when the cliff releases or the linear stream starts). */
export interface UnlockEvent {
  unlockDate: string;
  tokenName?: string;
  tokenSymbol?: string;
  dataSource?: string;
  linearUnlocks?: LinearUnlocks | null;
  cliffUnlocks?: CliffUnlocks;
  latestUpdateDate?: string;
  [field: string]: unknown;
}

/** One day's emissions, with their split over allocations. */
export interface DailyEmission {
  startDate: string;
  endDate: string;
  tokenName?: string;
  tokenSymbol?: string;
  unlockAmount?: number;
  unlockValue?: number;
  referencePrice?: number;
  allocations?: { allocationName?: string; standardAllocationName?: string; unlockAmount?: number; unlockValue?: number; [field: string]: unknown }[];
  [field: string]: unknown;
}

/** One fundraising round. */
export interface FundingRound {
  fundingRound?: string;
  fundingDate?: string | null;
  raiseType?: string | null;
  amountRaised?: number;
  valuation?: number;
  pricePerToken?: number;
  sourceURLList?: string[];
  investors?: { name?: string; [field: string]: unknown }[];
  numberOfInvestors?: number;
  [field: string]: unknown;
}

/** A token's fundraising history. */
export interface Fundraising {
  tokenId?: string;
  tokenName?: string;
  tokenSymbol?: string;
  listedMethod?: string;
  totalAmountRaised?: number;
  unlockedInvestor?: number | null;
  investorReleasedPercentage?: number | null;
  fundingRounds?: FundingRound[];
  [field: string]: unknown;
}

async function request(
  ctx: Ctx,
  path: string,
  params?: Params,
): Promise<{ metadata?: Metadata; data: any }> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.TOKENOMIST_API_KEY) headers["x-api-key"] = ctx.env.TOKENOMIST_API_KEY;
  const res = await ctx.fetch(
    (ctx.env.TOKENOMIST_API_BASE_URL || DEFAULT_BASE) + path + query(params ?? {}),
    { headers },
  );
  const text = await res.text();
  let body: any;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(`Tokenomist returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  }
  if (body?.status === false) {
    throw new Error(`Tokenomist: ${body.errorMessage ?? JSON.stringify(body).slice(0, 1000)}`);
  }
  if (!res.ok) throw new Error(`Tokenomist returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return body;
}

/**
 * Available tokens with supply, market cap, locked and unlocked totals. `limit` caps how many
 * of the first tokens are returned (default 50).
 */
export async function list_tokens(
  args: { limit?: number },
  ctx: Ctx,
): Promise<{ tokens: Token[]; metadata?: Metadata }> {
  const body = await request(ctx, "/v1/token/list");
  const tokens = (body.data ?? []) as Token[];
  return { tokens: tokens.slice(0, args.limit ?? 50), metadata: body.metadata };
}

/**
 * A token's allocation table: one row per allocation, split into `Scheduled` and `TBD`, with
 * unlocked, locked and total amounts, plus Tokenomist's comparable `standardAllocationName`
 * grouping.
 */
export async function get_allocations(
  args: { token_id: string },
  ctx: Ctx,
): Promise<{ allocations: Allocations; metadata?: Metadata }> {
  const body = await request(ctx, "/v1/allocations", { tokenId: args.token_id });
  return { allocations: body.data, metadata: body.metadata };
}

/**
 * Scheduled unlock events for a token, earliest first: each event carries a cliff release
 * (`cliffUnlocks`: amount, USD value, share of market cap) and/or the start of a linear stream
 * (`linearUnlocks`: total and daily amounts). `start`/`end` are ISO dates (YYYY-MM-DD).
 */
export async function get_unlock_events(
  args: { token_id: string; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ events: UnlockEvent[]; metadata?: Metadata }> {
  const body = await request(ctx, "/v1/unlock/events", {
    tokenId: args.token_id,
    start: args.start,
    end: args.end,
  });
  return { events: (body.data ?? []) as UnlockEvent[], metadata: body.metadata };
}

/**
 * Daily emission series for a token: per-day unlock amounts and USD values with their split
 * over allocations. `start`/`end` are ISO dates (YYYY-MM-DD) and default to the token's full
 * history.
 */
export async function get_daily_emissions(
  args: { token_id: string; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ emissions: DailyEmission[]; metadata?: Metadata }> {
  const body = await request(ctx, "/v1/daily-emission", {
    tokenId: args.token_id,
    start: args.start,
    end: args.end,
  });
  return { emissions: (body.data ?? []) as DailyEmission[], metadata: body.metadata };
}

/**
 * Fundraising history for a token: rounds with date, amount raised, valuation, price per token
 * and investors, plus totals. Dates are ISO (YYYY-MM-DD); `raised_amount_min`/`raised_amount_max`
 * filter on the amount raised in USD.
 */
export async function get_fundraising(
  args: {
    token_id: string;
    funding_date_min?: string;
    funding_date_max?: string;
    raised_amount_min?: number;
    raised_amount_max?: number;
  },
  ctx: Ctx,
): Promise<{ fundraising: Fundraising; metadata?: Metadata }> {
  const body = await request(ctx, `/v1/fundraising/token/${encodeURIComponent(args.token_id)}`, {
    fundingDateMin: args.funding_date_min,
    fundingDateMax: args.funding_date_max,
    raisedAmountMin: args.raised_amount_min,
    raisedAmountMax: args.raised_amount_max,
  });
  return { fundraising: body.data, metadata: body.metadata };
}
