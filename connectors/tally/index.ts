import type { Ctx } from "../centaur";

// Results are Tally's own GraphQL objects, passed through; interfaces list the
// fields worth knowing. Every call is a POST to the single GraphQL endpoint with
// an `Api-Key` header (injected by iron-proxy, or `TALLY_API_KEY` locally).
// GraphQL errors, including `api key required` on a 401, are thrown with Tally's
// message.

const DEFAULT_URL = "https://api.tally.xyz/query";

/** A DAO: one organization per governor family. */
export interface Organization {
  id: string;
  name: string;
  slug: string;
  chainIds?: (string | number)[];
  governorIds?: string[];
  proposalsCount?: number;
  delegatesCount?: number;
  tokenOwnersCount?: number;
  [field: string]: unknown;
}

/** A governor contract (one chain, one DAO). */
export interface Governor {
  id: string;
  name: string;
  slug: string;
  chainId?: string | number;
  proposalCount?: number;
  organization?: { name?: string; slug?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** One option's tally inside a proposal. */
export interface VoteStats {
  type: string;
  votesCount?: string | number;
  votersCount?: number;
  percent?: number;
  [field: string]: unknown;
}

/** A governance proposal with its vote stats and voting window. */
export interface Proposal {
  id: string;
  onchainId?: string;
  status?: string;
  metadata?: { title?: string; description?: string; [field: string]: unknown };
  voteStats?: VoteStats[];
  start?: { timestamp?: string | number; [field: string]: unknown };
  end?: { timestamp?: string | number; [field: string]: unknown };
  governor?: { name?: string; [field: string]: unknown };
  organization?: { name?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** A delegate: an account holding voting power on behalf of others. */
export interface Delegate {
  id: string;
  account?: { address?: string; name?: string | null; ens?: string | null; [field: string]: unknown };
  votesCount?: string | number;
  delegatorsCount?: number;
  organization?: { name?: string; [field: string]: unknown };
  [field: string]: unknown;
}

const ORGANIZATION_FIELDS = `
  id
  name
  slug
  chainIds
  governorIds
  proposalsCount
  delegatesCount
  tokenOwnersCount
`;

const PROPOSAL_FIELDS = `
  id
  onchainId
  status
  metadata { title description }
  voteStats { votesCount votersCount type percent }
  start { timestamp }
  end { timestamp }
  governor { name }
  organization { name }
`;

async function graphql(
  ctx: Ctx,
  query: string,
  variables: Record<string, unknown>,
): Promise<Record<string, any>> {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    accept: "application/json",
  };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.TALLY_API_KEY) headers["Api-Key"] = ctx.env.TALLY_API_KEY;
  const res = await ctx.fetch(ctx.env.TALLY_API_URL || DEFAULT_URL, {
    method: "POST",
    headers,
    body: JSON.stringify({ query, variables }),
  });
  const text = await res.text();
  let body: any;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(`Tally returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  }
  const errors = body?.errors as { message?: string }[] | undefined;
  if (errors?.length) {
    throw new Error(
      `Tally: ${errors.map((error) => error.message ?? JSON.stringify(error)).join("; ")}`,
    );
  }
  if (!res.ok) throw new Error(`Tally returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return body.data ?? {};
}

/**
 * Governance organizations (DAOs) with their chains, governors, and proposal, delegate and
 * token-holder counts. `limit` is the page size (default 20).
 */
export async function list_organizations(
  args: { limit?: number },
  ctx: Ctx,
): Promise<{ organizations: Organization[] }> {
  const data = await graphql(
    ctx,
    `query Organizations($input: OrganizationsInput!) {
      organizations(input: $input) { nodes { ${ORGANIZATION_FIELDS} } }
    }`,
    { input: { page: { limit: args.limit ?? 20 } } },
  );
  return { organizations: data.organizations?.nodes ?? [] };
}

/** One organization by slug (for example `uniswap`); `null` when no such slug exists. */
export async function get_organization(
  args: { slug: string },
  ctx: Ctx,
): Promise<{ organization: Organization | null }> {
  const data = await graphql(
    ctx,
    `query Organization($input: OrganizationInput!) {
      organization(input: $input) { ${ORGANIZATION_FIELDS} }
    }`,
    { input: { slug: args.slug } },
  );
  return { organization: data.organization ?? null };
}

/** Governors, optionally restricted to one organization slug, with chain and proposal count. */
export async function list_governors(
  args: { organization_slug?: string; limit?: number },
  ctx: Ctx,
): Promise<{ governors: Governor[] }> {
  const input: Record<string, unknown> = { page: { limit: args.limit ?? 20 } };
  if (args.organization_slug) input.organizationSlug = args.organization_slug;
  const data = await graphql(
    ctx,
    `query Governors($input: GovernorsInput!) {
      governors(input: $input) {
        nodes {
          id
          name
          slug
          chainId
          proposalCount
          organization { name slug }
        }
      }
    }`,
    { input },
  );
  return { governors: data.governors?.nodes ?? [] };
}

/**
 * Proposals filtered by `governor_id` and/or `organization_slug`, with status, title,
 * description, per-option vote stats, and the voting window (`start`/`end` are unix seconds).
 * `limit` is the page size (default 20).
 */
export async function list_proposals(
  args: { governor_id?: string; organization_slug?: string; limit?: number },
  ctx: Ctx,
): Promise<{ proposals: Proposal[] }> {
  const input: Record<string, unknown> = { page: { limit: args.limit ?? 20 } };
  if (args.governor_id) input.governorId = args.governor_id;
  if (args.organization_slug) input.organizationSlug = args.organization_slug;
  const data = await graphql(
    ctx,
    `query Proposals($input: ProposalsInput!) {
      proposals(input: $input) { nodes { ${PROPOSAL_FIELDS} } }
    }`,
    { input },
  );
  return { proposals: data.proposals?.nodes ?? [] };
}

/** One proposal by its Tally id; `null` when it does not exist. */
export async function get_proposal(
  args: { proposal_id: string },
  ctx: Ctx,
): Promise<{ proposal: Proposal | null }> {
  const data = await graphql(
    ctx,
    `query Proposal($input: ProposalInput!) {
      proposal(input: $input) { ${PROPOSAL_FIELDS} }
    }`,
    { input: { id: args.proposal_id } },
  );
  return { proposal: data.proposal ?? null };
}

/**
 * Delegates of one organization, largest first, with account address, ENS name and delegator
 * count. `sort_by` is `votes` (default) or `delegators`.
 */
export async function list_delegates(
  args: { organization_slug: string; limit?: number; sort_by?: "votes" | "delegators" },
  ctx: Ctx,
): Promise<{ delegates: Delegate[] }> {
  const sortBy = args.sort_by === "delegators" ? "DELEGATORS" : "VOTES";
  const data = await graphql(
    ctx,
    `query Delegates($input: DelegatesInput!) {
      delegates(input: $input) {
        nodes {
          id
          account { address name ens }
          votesCount
          delegatorsCount
          organization { name }
        }
      }
    }`,
    {
      input: {
        organizationSlug: args.organization_slug,
        page: { limit: args.limit ?? 20 },
        sort: { sortBy, isDescending: true },
      },
    },
  );
  return { delegates: data.delegates?.nodes ?? [] };
}
