import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are Preqin's own objects, passed through. The interfaces list the
// fields worth knowing; Preqin may send more. Operational API field names are
// PascalCase in requests and may be PascalCase or camelCase in responses.

const OPERATIONAL_BASE = "https://api.preqin.com";
const FEEDS_BASE = "https://feeds.preqin.com";

/** A Preqin fund manager (firm). */
export interface FundManager {
  FundManagerID?: string | number;
  FundManagerId?: string | number;
  FundManagerName?: string;
  [field: string]: unknown;
}

/** A Preqin fund. */
export interface Fund {
  FundId?: string | number;
  FundID?: string | number;
  FundName?: string;
  FundManagerName?: string;
  [field: string]: unknown;
}

/** A public Preqin Feeds API OpenAPI spec. */
export interface FeedSpec {
  version?: string;
  url?: string;
  [field: string]: unknown;
}

/** The Operational API's fund-manager payload, either a list or an envelope containing one. */
export type FundManagerResponse =
  | FundManager[]
  | {
      data?: FundManager[];
      items?: FundManager[];
      results?: FundManager[];
      fundManagers?: FundManager[];
      fundManager?: FundManager[];
      [field: string]: unknown;
    };

/** The Operational API's fund payload, either a list or an envelope containing one. */
export type FundResponse =
  | Fund[]
  | {
      data?: Fund[];
      items?: Fund[];
      results?: Fund[];
      funds?: Fund[];
      [field: string]: unknown;
    };

function cleanSecret(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  if (!trimmed.includes("\n")) return trimmed;
  for (const line of trimmed.split("\n")) {
    const candidate = line.trim();
    if (candidate && !candidate.startsWith("===") && !candidate.startsWith("#")) {
      return candidate;
    }
  }
  return undefined;
}

function list<T>(body: unknown, keys: string[]): T[] {
  if (Array.isArray(body)) return body as T[];
  if (body && typeof body === "object") {
    for (const key of keys) {
      const value = (body as Record<string, unknown>)[key];
      if (Array.isArray(value)) return value as T[];
    }
  }
  return [];
}

async function acquireToken(ctx: Ctx, username: string, apiKey: string): Promise<string> {
  const boundary = "----centaur-preqin-token-boundary";
  const body = [
    `--${boundary}`,
    'Content-Disposition: form-data; name="username"',
    "",
    username,
    `--${boundary}`,
    'Content-Disposition: form-data; name="apikey"',
    "",
    apiKey,
    `--${boundary}--`,
    "",
  ].join("\r\n");
  const res = await ctx.fetch(`${OPERATIONAL_BASE}/connect/token`, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": `multipart/form-data; boundary=${boundary}`,
    },
    body,
  });
  const text = await res.text();
  if (!res.ok) {
    const detail = text.trim() ? ` - ${text.slice(0, 1000)}` : "";
    throw new Error(
      `Preqin Operational API auth failed (${res.status}) at /connect/token using username/api key${detail}`,
    );
  }
  let payload: Record<string, unknown>;
  try {
    payload = JSON.parse(text);
  } catch {
    throw new Error("Preqin Operational API auth response was not JSON");
  }
  const token = payload.access_token ?? payload.accessToken ?? payload.token;
  if (typeof token !== "string" || !token) {
    throw new Error("Preqin Operational API auth response did not include an access token");
  }
  return token;
}

async function operationalToken(ctx: Ctx): Promise<string | undefined> {
  const injected = cleanSecret(ctx.env.PREQIN_OPERATIONAL_TOKEN);
  if (injected) return injected;
  const username = cleanSecret(ctx.env.PREQIN_USERNAME);
  const apiKey = cleanSecret(ctx.env.PREQIN_API_KEY);
  if (!username || !apiKey) return undefined;
  return acquireToken(ctx, username, apiKey);
}

async function operationalGet(
  ctx: Ctx,
  endpoint: string,
  params: Params = {},
): Promise<any> {
  const token = await operationalToken(ctx);
  const headers: Record<string, string> = { accept: "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const res = await ctx.fetch(`${OPERATIONAL_BASE}${endpoint}${query(params)}`, { headers });
  const text = await res.text();
  if (!res.ok) {
    const detail = text.trim() ? ` - ${text.slice(0, 1000)}` : "";
    throw new Error(`Preqin API error (${res.status}) for ${endpoint}${detail}`);
  }
  return text ? JSON.parse(text) : null;
}

async function feedsGet(ctx: Ctx, path: string): Promise<any> {
  const res = await ctx.fetch(`${FEEDS_BASE}${path}`, {
    headers: { accept: "application/json" },
  });
  const text = await res.text();
  if (!res.ok) {
    const detail = text.trim() ? ` - ${text.slice(0, 1000)}` : "";
    throw new Error(`Preqin Feeds API returned HTTP ${res.status} for ${path}${detail}`);
  }
  return text ? JSON.parse(text) : null;
}

/**
 * Search Preqin fund managers (firms). `fund_manager_name` is a name search, `fund_manager_id`
 * looks up one firm, `asset_class` is an asset-class code such as `pe`, `re` or `hf`, and
 * `include` requests asset-class-specific data. `size` is records per page (max 200, default 20)
 * and `page` starts at 1.
 */
export async function get_fund_managers(
  args: {
    fund_manager_name?: string;
    fund_manager_id?: string;
    asset_class?: string;
    include?: string;
    size?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<{ fund_managers: FundManagerResponse }> {
  const body = await operationalGet(ctx, "/api/FundManager", {
    FundManagerName: args.fund_manager_name,
    FundManagerID: args.fund_manager_id,
    AssetClass: args.asset_class,
    Include: args.include,
    Size: args.size ?? 20,
    Page: args.page ?? 1,
  });
  return { fund_managers: body };
}

/**
 * Search Preqin funds. Filter by `fund_name`, `fund_id`, `fund_manager_name`, `fund_manager_id`,
 * `asset_class` (`pe`, `re`, `hf`, ...), `strategy` or `status`; `include` requests
 * asset-class-specific data. `size` is records per page (max 200, default 20) and `page` starts
 * at 1.
 */
export async function get_funds(
  args: {
    fund_name?: string;
    fund_id?: string;
    fund_manager_name?: string;
    fund_manager_id?: string;
    asset_class?: string;
    strategy?: string;
    status?: string;
    include?: string;
    size?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<{ funds: FundResponse }> {
  const body = await operationalGet(ctx, "/api/Fund", {
    FundName: args.fund_name,
    FundId: args.fund_id,
    FundManagerName: args.fund_manager_name,
    FundManagerId: args.fund_manager_id,
    AssetClass: args.asset_class,
    Strategy: args.strategy,
    Status: args.status,
    Include: args.include,
    Size: args.size ?? 20,
    Page: args.page ?? 1,
  });
  return { funds: body };
}

/** List the public Preqin Feeds API OpenAPI spec versions, with `version` and `url`. */
export async function list_feed_specs(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<{ specs: FeedSpec[] }> {
  const body = await feedsGet(ctx, "/OpenApiSpecs");
  return { specs: list<FeedSpec>(body, ["specs", "data", "results"]) };
}
