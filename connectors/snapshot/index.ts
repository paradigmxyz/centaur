import type { Ctx } from "../centaur";

// Snapshot's Hub GraphQL API (https://hub.snapshot.org/graphql). Results are the
// GraphQL fields the query selects, passed through with Snapshot's own names.

const DEFAULT_URL = "https://hub.snapshot.org/graphql";

/** A governance space, such as `aave.eth`. */
export interface Space {
  id: string;
  name?: string;
  about?: string;
  /** Chain id as a string ("1" for Ethereum mainnet). */
  network?: string;
  symbol?: string;
  /** Member addresses. */
  members?: string[];
  admins?: string[];
  strategies?: Strategy[];
  proposalsCount?: number;
  followersCount?: number;
  [field: string]: unknown;
}

/** One voting strategy attached to a space. */
export interface Strategy {
  name: string;
  network?: string;
  params?: Record<string, unknown>;
  [field: string]: unknown;
}

/** A proposal. `start` and `end` are unix seconds; `scores` aligns with `choices`. */
export interface Proposal {
  id: string;
  title?: string;
  body?: string;
  choices?: string[];
  start?: number;
  end?: number;
  /** Block number the votes are counted at. */
  snapshot?: number;
  /** `pending`, `active` or `closed`. */
  state?: string;
  author?: string;
  scores?: number[];
  scores_total?: number;
  votes?: number;
  space?: { id: string; name?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** A vote on a proposal. `choice` is an index into `choices`, or an index/weight map for
 * weighted and approval voting; `vp` is the voter's voting power. */
export interface Vote {
  id: string;
  voter?: string;
  vp?: number;
  choice?: number | Record<string, number>;
  created?: number;
  space?: { id: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** Voting power of one voter in one space/proposal context. */
export interface VotingPower {
  vp?: number;
  vp_by_strategy?: number[];
  vp_state?: string;
  [field: string]: unknown;
}

interface GraphQLBody {
  data?: Record<string, unknown>;
  errors?: { message?: string; [field: string]: unknown }[];
}

async function graphql(
  ctx: Ctx,
  query: string,
  variables: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  const headers: Record<string, string> = {
    accept: "application/json",
    "content-type": "application/json",
  };
  // Behind iron-proxy the optional key is injected on the wire; locally it comes from the env.
  if (ctx.env.SNAPSHOT_API_KEY) headers["x-api-key"] = ctx.env.SNAPSHOT_API_KEY;
  const res = await ctx.fetch(ctx.env.SNAPSHOT_API_URL || DEFAULT_URL, {
    method: "POST",
    headers,
    body: JSON.stringify({ query, variables }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Snapshot returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  const body = JSON.parse(text) as GraphQLBody;
  if (body.errors?.length) {
    const message = body.errors.map((error) => error.message ?? JSON.stringify(error)).join("; ");
    throw new Error(`Snapshot GraphQL error: ${message}`);
  }
  return body.data ?? {};
}

/**
 * One space by id (for example `aave.eth`): name, about, network, symbol, members, admins,
 * voting strategies, proposal and follower counts. `space` is null when the id is unknown.
 */
export async function get_space(
  args: { space_id: string },
  ctx: Ctx,
): Promise<{ space: Space | null }> {
  const query = `
    query Space($id: String!) {
      space(id: $id) {
        id name about network symbol members admins
        strategies { name network params }
        proposalsCount followersCount
      }
    }`;
  const data = await graphql(ctx, query, { id: args.space_id });
  return { space: (data.space ?? null) as Space | null };
}

/**
 * List spaces, most recently created first by default. `order_by` is a space field such as
 * `created`, `proposalsCount` or `followersCount`; `order_direction` is `asc` or `desc`;
 * `first` is the page size (cap it and increase it to page forward).
 */
export async function list_spaces(
  args: { first?: number; order_by?: string; order_direction?: "asc" | "desc" },
  ctx: Ctx,
): Promise<{ spaces: Space[] }> {
  const query = `
    query Spaces($first: Int!, $orderBy: String!, $orderDirection: OrderDirection!) {
      spaces(first: $first, orderBy: $orderBy, orderDirection: $orderDirection) {
        id name about network symbol proposalsCount followersCount
      }
    }`;
  const data = await graphql(ctx, query, {
    first: args.first ?? 20,
    orderBy: args.order_by ?? "created",
    orderDirection: args.order_direction ?? "desc",
  });
  return { spaces: (data.spaces ?? []) as Space[] };
}

/**
 * One proposal by id (`0x…` hex): title, full body markdown, choices, start and end (unix
 * seconds), snapshot block, state, author, current `scores` (one per choice), `scores_total`
 * and vote count. `proposal` is null when the id is unknown.
 */
export async function get_proposal(
  args: { proposal_id: string },
  ctx: Ctx,
): Promise<{ proposal: Proposal | null }> {
  const query = `
    query Proposal($id: String!) {
      proposal(id: $id) {
        id title body choices start end snapshot state author
        scores scores_total votes
        space { id name }
      }
    }`;
  const data = await graphql(ctx, query, { id: args.proposal_id });
  return { proposal: (data.proposal ?? null) as Proposal | null };
}

/**
 * Proposals of one space, newest first by `created`. `state` filters to `active`, `closed`,
 * `pending` or `all` (omit it for any state); `first` is the page size.
 */
export async function list_proposals(
  args: { space: string; state?: string; first?: number },
  ctx: Ctx,
): Promise<{ proposals: Proposal[] }> {
  const query = `
    query Proposals($space: String!, $state: String, $first: Int!) {
      proposals(
        first: $first
        where: { space: $space, state: $state }
        orderBy: "created"
        orderDirection: desc
      ) {
        id title choices start end state author
        scores scores_total votes
        space { id name }
      }
    }`;
  const variables: Record<string, unknown> = { space: args.space, first: args.first ?? 20 };
  if (args.state) variables.state = args.state;
  const data = await graphql(ctx, query, variables);
  return { proposals: (data.proposals ?? []) as Proposal[] };
}

/**
 * Votes on a proposal, largest voting power first: id, voter address, `vp`, `choice` (an index
 * into the proposal's `choices`, or an index/weight map for weighted voting) and creation time
 * (unix seconds). `first` is the page size, at most 1000.
 */
export async function get_votes(
  args: { proposal_id: string; first?: number },
  ctx: Ctx,
): Promise<{ votes: Vote[] }> {
  const query = `
    query Votes($proposal: String!, $first: Int!) {
      votes(
        first: $first
        where: { proposal: $proposal }
        orderBy: "vp"
        orderDirection: desc
      ) {
        id voter vp choice created space { id }
      }
    }`;
  const data = await graphql(ctx, query, { proposal: args.proposal_id, first: args.first ?? 1000 });
  return { votes: (data.votes ?? []) as Vote[] };
}

/**
 * Voting power of `voter` (a 0x address) in `space` as of `proposal`: `vp` total and
 * `vp_by_strategy` broken down per space strategy.
 */
export async function get_voting_power(
  args: { voter: string; space: string; proposal: string },
  ctx: Ctx,
): Promise<{ voting_power: VotingPower }> {
  const query = `
    query VotingPower($voter: String!, $space: String!, $proposal: String!) {
      vp(voter: $voter, space: $space, proposal: $proposal) {
        vp vp_by_strategy vp_state
      }
    }`;
  const data = await graphql(ctx, query, {
    voter: args.voter,
    space: args.space,
    proposal: args.proposal,
  });
  return { voting_power: data.vp as VotingPower };
}
