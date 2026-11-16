import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Karma's own objects, passed through after unwrapping its
// `{ data }` envelope. Field names are Karma's (https://api.karmahq.xyz,
// https://docs.karmahq.xyz). Public governance data needs no key.

const BASE = "https://api.karmahq.xyz/api";

/** A DAO Karma tracks. */
export interface Dao {
  /** Short name used as `dao_name`, for example "ens". */
  name: string;
  fullName?: string;
  description?: string;
  logoUrl?: string;
  forumTopicURL?: string;
  tokenAddress?: string[];
  chainId?: number;
  [field: string]: unknown;
}

/** A delegate's scores and activity for one period (`lifetime`). */
export interface DelegateStats {
  period: string;
  /** Karma reputation score (0–100). */
  karmaScore?: number | null;
  karmaRank?: number | null;
  forumActivityScore?: number | null;
  forumLikesReceived?: number | null;
  forumPostsReadCount?: number | null;
  forumTopicCount?: number | null;
  forumPostCount?: number | null;
  proposalsInitiated?: number | null;
  proposalsDiscussed?: number | null;
  proposalsOnSnapshot?: number | null;
  onChainVotesPct?: number | null;
  offChainVotesPct?: number | null;
  delegateVotesPct?: number | null;
  percentile?: number | null;
  /** ISO 8601. */
  updatedAt?: string;
  createdAt?: string;
  [field: string]: unknown;
}

/** A DAO delegate with profile, reputation score and delegation figures. */
export interface Delegate {
  /** Ethereum address (lowercase hex). */
  publicAddress: string;
  daoName?: string;
  ensName?: string | null;
  twitterHandle?: string | null;
  discourseHandles?: string[];
  isForumVerified?: boolean;
  isNominee?: boolean;
  /** The delegate's Karma score, the value `stats[].karmaScore` reports. */
  score?: number | null;
  /** Other DAOs this address delegates in, as a comma-separated string. */
  daos?: string;
  daoCount?: number;
  userType?: string;
  pitch?: string | null;
  stats?: DelegateStats[];
  delegatorCount?: number;
  /** Voting power in whole token units, as a string (not wei). */
  delegatedVotes?: string | null;
  snapshotDelegatedVotes?: string | null;
  voteWeight?: string | null;
  /** ISO 8601. */
  firstTokenDelegatedAt?: string | null;
  joinDateAt?: string | null;
  workstreams?: unknown[];
  scoreBreakdown?: { name: string; score?: number; pct?: number; [field: string]: unknown }[];
  aboutMe?: string | null;
  realName?: string | null;
  profilePicture?: string | null;
  website?: string | null;
  delegatePitch?: string | null;
  status?: string;
  roles?: unknown[];
  [field: string]: unknown;
}

/**
 * A DAO's delegate page: the DAO profile (`name`, `fullName`, `description`, `daos`,
 * `socialLinks`), its totals (`count` delegates, `delegateCount30dActivity`) and a `delegates` page.
 */
export interface DaoDelegates {
  name?: string;
  fullName?: string;
  description?: string;
  forumTopicURL?: string;
  /** Total delegates in the DAO, independent of the requested page. */
  count?: number;
  delegateCount30dActivity?: number;
  daos?: Dao[];
  socialLinks?: Record<string, unknown>;
  delegates: Delegate[];
  onChainId?: string;
  snapshotIds?: string[];
  [field: string]: unknown;
}

/** `find-delegate`: the delegate plus the DAO it was looked up in and its sibling DAOs. */
export interface DelegateProfile {
  name?: string;
  delegate: Delegate;
  daos?: Dao[];
  [field: string]: unknown;
}

async function request(ctx: Ctx, endpoint: string, params: Record<string, string | number> = {}): Promise<any> {
  const res = await ctx.fetch(`${BASE}${endpoint}${query(params)}`, {
    headers: { accept: "application/json" },
  });
  const body = await readJson(res, "Karma");
  return body && typeof body === "object" && body.data !== undefined ? body.data : body;
}

/**
 * Every DAO Karma tracks, with short `name` (used as `dao_name` elsewhere), full name, description,
 * forum link, logo and token addresses. Discovery only: the public API currently answers with a
 * single DAO (for example "ens") regardless of page size, and other `dao_name` values fail with
 * "Dao not found", so pass a name this call returns.
 */
export async function list_daos(args: {}, ctx: Ctx): Promise<{ daos: Dao[] }> {
  const data = await request(ctx, "/dao", { pageSize: 100 });
  return { daos: data.daos ?? [] };
}

/**
 * One page of a DAO's delegates, highest Karma score first by default, with profile, ens name,
 * `score`, `stats` (lifetime karma score, rank, forum activity, proposal and voting percentages),
 * delegator count and delegated voting power (whole token units as a string). `dao_name` is the
 * short DAO name from `list_daos` (currently "ens"); `limit` is the page size (default 20),
 * `offset` skips delegates, and `order` sorts by score ascending or descending.
 */
export async function get_delegates(
  args: { dao_name: string; limit?: number; offset?: number; order?: "asc" | "desc" },
  ctx: Ctx,
): Promise<DaoDelegates> {
  const limit = args.limit ?? 20;
  if (limit < 1) throw new Error("limit must be at least 1");
  return request(ctx, "/dao/delegates", {
    name: args.dao_name.trim(),
    pageSize: limit,
    offset: args.offset ?? 0,
    order: args.order ?? "desc",
  });
}

/**
 * One delegate's profile and stats in a DAO: identity links, `score`, `stats`, `scoreBreakdown`,
 * delegator count, delegated votes and the other DAOs they delegate in. `address` is the delegate's
 * Ethereum address (0x…); `dao_name` is a short DAO name from `list_daos` (currently "ens"). An address
 * that is not a delegate in that DAO fails with Karma's "Delegate not found".
 */
export async function get_delegate(
  args: { dao_name: string; address: string },
  ctx: Ctx,
): Promise<DelegateProfile> {
  return request(ctx, "/dao/find-delegate", {
    dao: args.dao_name.trim(),
    user: args.address.trim(),
  });
}

/**
 * Governance stats for one DAO: the DAO profile, total delegate `count`, delegates active in the
 * last 30 days (`delegateCount30dActivity`), and the top delegate. It is the delegate listing with
 * a page size of one, so read the totals from the top level, not from `delegates`.
 */
export async function get_dao_stats(args: { dao_name: string }, ctx: Ctx): Promise<DaoDelegates> {
  return request(ctx, "/dao/delegates", { name: args.dao_name.trim(), pageSize: 1 });
}
