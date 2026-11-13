import type { Ctx } from "../centaur";

// OpenTable search through Browser Use cloud browsers. OpenTable has no public
// availability API, so a Browser Use agent loads the search page and reads the
// restaurant cards. That takes a minute or more, so this connector exposes
// start / status / results instead of blocking.

const DEFAULT_BASE = "https://api.browser-use.com/api/v2";

// A stored Browser Use profile keeps OpenTable cookies (region/consent state)
// between runs; the proxy country pins the session to a US IP.
const DEFAULT_PROFILE_ID = "1da1d19b-4fa4-4f7a-bc2e-f3854df9db0b";
const DEFAULT_PROXY_COUNTRY = "us";
const DEFAULT_MAX_STEPS = 15;
const DEFAULT_METRO = "sf";

const OPENTABLE_SEARCH = "https://www.opentable.com/s";

/** A zipcode with the OpenTable IDs that narrow a search to its neighborhoods. */
export interface ZipcodeInfo {
  name: string;
  /** `neighborhoodIds` values. */
  ids: number[];
  metro_id: number;
  region_id: number;
}

/** A mapped zipcode as listed by `list_zipcodes`. */
export interface ZipcodeEntry extends ZipcodeInfo {
  zipcode: string;
}

/** A metro area (city) with its OpenTable `metroId`. */
export interface Metro {
  /** Short key such as `sf`, `nyc` or `la`. */
  key: string;
  /** `metroId` for the search URL. */
  id: number;
  name: string;
}

/** A region within a metro (borough, county, sub-city area). */
export interface Region {
  key: string;
  /** `regionId`, or null for "all regions". */
  id: number | null;
  name: string;
}

/** A neighborhood within a region. */
export interface Neighborhood {
  name: string;
  /** `neighborhoodId`. */
  id: number;
}

/** One restaurant card from the OpenTable search page. */
export interface Restaurant {
  name?: string;
  cuisine?: string;
  /** Neighborhood or location line. */
  neighborhood?: string;
  /** Clickable reservation times, such as `7:00 PM`. */
  time_slots?: string[];
  rating?: string | number;
  price_range?: string;
  [field: string]: unknown;
}

/** The OpenTable search URL plus the IDs it was built from. */
export interface SearchSetup {
  url: string;
  metro_id: number;
  region_ids: number[];
  neighborhood_ids: number[];
  /** Local start time, `YYYY-MM-DDTHH:MM:SS`. */
  date_time: string;
  /** Where the metro, region and neighborhood IDs came from. */
  resolved_from: string;
}

/** A started Browser Use task. */
export interface StartedTask {
  task_id: string;
  session_id: string;
  setup: SearchSetup;
}

/** A Browser Use task status: `created`, `started`, `finished`, `failed` or `stopped`. */
export interface TaskStatus {
  id: string;
  status: "created" | "started" | "finished" | "failed" | "stopped";
  /** The agent's final output; null until the task finishes. */
  output?: string | null;
  finishedAt?: string | null;
  isSuccess?: boolean | null;
  [field: string]: unknown;
}

// OpenTable location IDs, mapped to the search URL's metroId, regionIds[] and
// neighborhoodIds[] parameters. Only the mapped metros can be used by key; pass
// raw IDs for anywhere else.
const ZIPCODES: Record<string, ZipcodeInfo> = {
  "94102": { name: "Civic Center / Hayes Valley / Tenderloin", ids: [34, 35, 21317], metro_id: 4, region_id: 5 },
  "94103": { name: "SoMa", ids: [45], metro_id: 4, region_id: 5 },
  "94104": { name: "Financial District", ids: [227], metro_id: 4, region_id: 5 },
  "94105": { name: "South Beach / Rincon Hill / Embarcadero", ids: [227, 45], metro_id: 4, region_id: 5 },
  "94107": { name: "Potrero Hill / Dogpatch / South Beach", ids: [45, 4235, 3287], metro_id: 4, region_id: 5 },
  "94108": { name: "Chinatown / Nob Hill", ids: [40, 10637], metro_id: 4, region_id: 5 },
  "94109": { name: "Russian Hill / Polk Gulch / Nob Hill", ids: [641, 40], metro_id: 4, region_id: 5 },
  "94110": { name: "Mission District", ids: [2951, 39], metro_id: 4, region_id: 5 },
  "94111": { name: "Embarcadero / Financial District", ids: [227], metro_id: 4, region_id: 5 },
  "94112": { name: "Excelsior / Outer Mission", ids: [], metro_id: 4, region_id: 5 },
  "94114": { name: "Castro / Noe Valley", ids: [32, 631], metro_id: 4, region_id: 5 },
  "94115": { name: "Pacific Heights / Japantown", ids: [42, 3029, 693], metro_id: 4, region_id: 5 },
  "94116": { name: "Sunset", ids: [640], metro_id: 4, region_id: 5 },
  "94117": { name: "Haight-Ashbury", ids: [1382], metro_id: 4, region_id: 5 },
  "94118": { name: "Inner Richmond", ids: [44], metro_id: 4, region_id: 5 },
  "94121": { name: "Outer Richmond", ids: [44], metro_id: 4, region_id: 5 },
  "94122": { name: "Inner Sunset", ids: [640], metro_id: 4, region_id: 5 },
  "94123": { name: "Marina / Cow Hollow", ids: [38, 22615], metro_id: 4, region_id: 5 },
  "94124": { name: "Bayview / Hunters Point", ids: [8285], metro_id: 4, region_id: 5 },
  "94127": { name: "West Portal / Forest Hill", ids: [916], metro_id: 4, region_id: 5 },
  "94129": { name: "Presidio", ids: [38, 674], metro_id: 4, region_id: 5 },
  "94131": { name: "Twin Peaks / Glen Park", ids: [4418], metro_id: 4, region_id: 5 },
  "94132": { name: "Lake Merced / Stonestown", ids: [], metro_id: 4, region_id: 5 },
  "94133": { name: "North Beach / Telegraph Hill", ids: [41], metro_id: 4, region_id: 5 },
  "94134": { name: "Visitacion Valley", ids: [], metro_id: 4, region_id: 5 },
  "94158": { name: "Mission Bay", ids: [4235, 45], metro_id: 4, region_id: 5 },
  "10001": { name: "Chelsea", ids: [100], metro_id: 8, region_id: 52 },
  "10002": { name: "Lower East Side", ids: [108], metro_id: 8, region_id: 52 },
  "10003": { name: "East Village / Gramercy", ids: [102, 104], metro_id: 8, region_id: 52 },
  "10004": { name: "Financial District", ids: [103], metro_id: 8, region_id: 52 },
  "10005": { name: "Financial District", ids: [103], metro_id: 8, region_id: 52 },
  "10006": { name: "Financial District", ids: [103], metro_id: 8, region_id: 52 },
  "10007": { name: "Tribeca", ids: [111], metro_id: 8, region_id: 52 },
  "10011": { name: "Chelsea / West Village", ids: [100, 114], metro_id: 8, region_id: 52 },
  "10012": { name: "SoHo / NoHo", ids: [110], metro_id: 8, region_id: 52 },
  "10013": { name: "Tribeca / SoHo", ids: [111, 110], metro_id: 8, region_id: 52 },
  "10014": { name: "West Village / Greenwich Village", ids: [114, 105], metro_id: 8, region_id: 52 },
  "10016": { name: "Gramercy / Murray Hill", ids: [104], metro_id: 8, region_id: 52 },
  "10017": { name: "Midtown East", ids: [109], metro_id: 8, region_id: 52 },
  "10018": { name: "Midtown / Times Square", ids: [109], metro_id: 8, region_id: 52 },
  "10019": { name: "Midtown West / Hell's Kitchen", ids: [109, 107], metro_id: 8, region_id: 52 },
  "10021": { name: "Upper East Side", ids: [112], metro_id: 8, region_id: 52 },
  "10022": { name: "Midtown East", ids: [109], metro_id: 8, region_id: 52 },
  "10023": { name: "Upper West Side", ids: [113], metro_id: 8, region_id: 52 },
  "10024": { name: "Upper West Side", ids: [113], metro_id: 8, region_id: 52 },
  "10025": { name: "Upper West Side", ids: [113], metro_id: 8, region_id: 52 },
  "10028": { name: "Upper East Side", ids: [112], metro_id: 8, region_id: 52 },
  "10036": { name: "Times Square / Hell's Kitchen", ids: [109, 107], metro_id: 8, region_id: 52 },
  "10065": { name: "Upper East Side", ids: [112], metro_id: 8, region_id: 52 },
  "10075": { name: "Upper East Side", ids: [112], metro_id: 8, region_id: 52 },
  "10128": { name: "Upper East Side", ids: [112], metro_id: 8, region_id: 52 },
};

const METROS: Record<string, { id: number; name: string }> = {
  sf: { id: 4, name: "San Francisco Bay Area" },
  san_francisco: { id: 4, name: "San Francisco Bay Area" },
  bay_area: { id: 4, name: "San Francisco Bay Area" },
  nyc: { id: 8, name: "New York City" },
  new_york: { id: 8, name: "New York City" },
  la: { id: 6, name: "Los Angeles" },
  los_angeles: { id: 6, name: "Los Angeles" },
};

const REGIONS: Record<string, Record<string, { id: number | null; name: string }>> = {
  "4": {
    all: { id: null, name: "All San Francisco Bay Area" },
    san_francisco: { id: 5, name: "San Francisco" },
    sf: { id: 5, name: "San Francisco" },
    east_bay: { id: 6, name: "East Bay" },
    marin: { id: 7, name: "Marin" },
    peninsula: { id: 8, name: "Peninsula" },
    south_bay: { id: 9, name: "San Jose / Silicon Valley" },
    silicon_valley: { id: 9, name: "San Jose / Silicon Valley" },
    wine_country: { id: 10, name: "Wine Country" },
    santa_cruz: { id: 11, name: "Santa Cruz / Capitola / Aptos" },
  },
  "8": {
    all: { id: null, name: "All New York City" },
    manhattan: { id: 52, name: "Manhattan" },
    brooklyn: { id: 53, name: "Brooklyn" },
    queens: { id: 54, name: "Queens" },
    bronx: { id: 55, name: "Bronx" },
    staten_island: { id: 56, name: "Staten Island" },
  },
  "6": {
    all: { id: null, name: "All Los Angeles" },
    hollywood: { id: 1048, name: "Hollywood" },
    downtown: { id: 1155, name: "Downtown" },
    westside: { id: 1591, name: "Westside" },
    beverly_hills: { id: 1822, name: "West Hollywood / Beverly Hills / Mid-Wilshire" },
    santa_monica: { id: 1405, name: "Beach Cities" },
    pasadena: { id: 627, name: "Pasadena" },
    valley: { id: 2195, name: "San Fernando Valley" },
  },
};

const NEIGHBORHOODS: Record<string, Record<string, number>> = {
  "5": {
    bayview: 8285,
    bernal_heights: 9110,
    chinatown: 10637,
    civic_center: 34,
    hayes_valley: 34,
    cole_valley: 36,
    cow_hollow: 22615,
    downtown: 35,
    union_square: 35,
    financial_district: 227,
    embarcadero: 227,
    fishermans_wharf: 49,
    forest_hill: 916,
    west_portal: 916,
    glen_park: 4418,
    haight: 1382,
    japantown: 693,
    marina: 38,
    presidio: 38,
    mission_bay: 4235,
    south_beach: 4235,
    mission: 2951,
    nob_hill: 40,
    noe_valley: 631,
    north_beach: 41,
    pacific_heights: 42,
    potrero_hill: 28802,
    dogpatch: 3287,
    richmond: 44,
    russian_hill: 641,
    soma: 45,
    sunset: 640,
    tenderloin: 21317,
    castro: 32,
    western_addition: 3029,
  },
  "52": {
    chelsea: 100,
    chinatown: 101,
    east_village: 102,
    financial_district: 103,
    gramercy: 104,
    greenwich_village: 105,
    harlem: 106,
    hells_kitchen: 107,
    lower_east_side: 108,
    midtown: 109,
    soho: 110,
    tribeca: 111,
    upper_east_side: 112,
    upper_west_side: 113,
    west_village: 114,
  },
};

function slug(value: string): string {
  return value.trim().toLowerCase().split(" ").join("_").split("-").join("_");
}

function base(ctx: Ctx): string {
  return (ctx.env.BROWSER_USE_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx): Record<string, string> {
  const value: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.BROWSER_USE_API_KEY) {
    value["Authorization"] = `Bearer ${ctx.env.BROWSER_USE_API_KEY}`;
  }
  return value;
}

/** Browser Use errors are `{ detail: { error, message } }` or `{ detail: "..." }`. */
function errorDetail(payload: any, text: string): string {
  const detail = payload?.detail;
  if (typeof detail === "string" && detail) return detail;
  if (detail && typeof detail === "object") {
    const message = detail.message || detail.error;
    if (typeof message === "string" && message) return message;
    return JSON.stringify(detail);
  }
  return text.trim();
}

async function call(
  ctx: Ctx,
  path: string,
  init: { method?: string; body?: unknown } = {},
): Promise<any> {
  const value = headers(ctx);
  if (init.body !== undefined) value["content-type"] = "application/json";
  const res = await ctx.fetch(`${base(ctx)}${path}`, {
    method: init.method || "GET",
    headers: value,
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
  });
  const text = await res.text();
  let payload: any;
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    payload = undefined;
  }
  if (!res.ok) {
    throw new Error(`Browser Use returned HTTP ${res.status}: ${errorDetail(payload, text)}`);
  }
  if (payload === undefined || payload === null) {
    throw new Error(`Browser Use returned non-JSON: ${text.slice(0, 1000)}`);
  }
  return payload;
}

function todayLocal(): string {
  const now = new Date();
  const pad = (value: number): string => (value < 10 ? "0" : "") + String(value);
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

function buildUrl(
  term: string,
  covers: number,
  date_time: string,
  metro_id: number,
  region_ids: number[],
  neighborhood_ids: number[],
  sort_by: string,
): string {
  let url =
    `${OPENTABLE_SEARCH}?dateTime=${encodeURIComponent(date_time)}` +
    `&covers=${encodeURIComponent(String(covers))}` +
    `&term=${encodeURIComponent(term)}` +
    `&intentModifiedTerm=${encodeURIComponent(term)}` +
    "&shouldUseLatLongSearch=true&showMap=true" +
    `&metroId=${encodeURIComponent(String(metro_id))}` +
    `&sortBy=${encodeURIComponent(sort_by)}`;
  for (const id of region_ids) url += `&regionIds[]=${encodeURIComponent(String(id))}`;
  for (const id of neighborhood_ids) url += `&neighborhoodIds[]=${encodeURIComponent(String(id))}`;
  return url;
}

/** Metro key, region key or zipcode to the IDs OpenTable's search URL wants. */
function resolveLocation(args: {
  zipcode?: string;
  metro?: string;
  region?: string;
  neighborhoods?: string[];
  metro_id?: number;
  region_ids?: number[];
  neighborhood_ids?: number[];
}): {
  metro_id: number;
  region_ids: number[];
  neighborhood_ids: number[];
  resolved_from: string;
} {
  if (args.metro_id !== undefined) {
    return {
      metro_id: args.metro_id,
      region_ids: args.region_ids ?? [],
      neighborhood_ids: args.neighborhood_ids ?? [],
      resolved_from: "explicit IDs",
    };
  }

  let metro_id: number | undefined;
  let region_ids: number[] | undefined;
  let neighborhood_ids: number[] | undefined;
  let resolved_from = "";

  if (args.zipcode) {
    const info = ZIPCODES[args.zipcode.trim()];
    if (!info) throw new Error(`Zipcode ${args.zipcode} is not mapped; pass metro or metro_id instead`);
    metro_id = info.metro_id;
    region_ids = [info.region_id];
    neighborhood_ids = info.ids;
    resolved_from = `zipcode ${args.zipcode} (${info.name})`;
  }

  const metro = slug(args.metro || DEFAULT_METRO);
  if (metro_id === undefined) {
    const found = METROS[metro];
    if (!found) {
      throw new Error(`Unknown metro "${metro}"; use one of: ${Object.keys(METROS).join(", ")}`);
    }
    metro_id = found.id;
    resolved_from = `metro ${metro}`;
  }

  const regions = REGIONS[String(metro_id)] ?? {};
  if (!region_ids && args.region) {
    const key = slug(args.region);
    const found = regions[key];
    if (!found) {
      throw new Error(
        `Unknown region "${key}" for metro ${metro_id}; use one of: ${Object.keys(regions).join(", ")}`,
      );
    }
    region_ids = found.id === null ? [] : [found.id];
  }

  if (!neighborhood_ids && args.neighborhoods?.length) {
    if (!region_ids?.length) {
      throw new Error("neighborhoods needs a single region (or zipcode) to resolve against");
    }
    const table = NEIGHBORHOODS[String(region_ids[0])];
    if (!table) {
      throw new Error(
        `No neighborhood IDs are mapped for region ${region_ids[0]}; pass neighborhood_ids instead`,
      );
    }
    const ids: number[] = [];
    const unknown: string[] = [];
    for (const name of args.neighborhoods) {
      const id = table[slug(name).split("'").join("")];
      if (id === undefined) unknown.push(name);
      else if (!ids.includes(id)) ids.push(id);
    }
    if (unknown.length) {
      throw new Error(
        `Unknown neighborhoods for region ${region_ids[0]}: ${unknown.join(", ")}; ` +
          `use one of: ${Object.keys(table).join(", ")}`,
      );
    }
    neighborhood_ids = ids;
  }

  return {
    metro_id,
    region_ids: region_ids ?? [],
    neighborhood_ids: neighborhood_ids ?? [],
    resolved_from,
  };
}

function searchTask(url: string, limit: number): string {
  return `1. Navigate to ${url}
2. Wait 5 seconds for page to fully load
3. Extract restaurant information from the search results. For each restaurant card, extract:
   - Restaurant name
   - Cuisine type
   - Neighborhood/location
   - Available time slots (the clickable reservation times shown)
   - Rating if available
   - Price range if available
4. Format the output as a JSON array with objects containing: name, cuisine, neighborhood, time_slots (array), rating, price_range
5. Only extract the first ${limit} restaurants
6. Return ONLY the JSON array, nothing else`;
}

/** The JSON array (or list of objects) an agent wrote, tolerating fences and sloppy quoting. */
function parseRestaurants(output: string): Restaurant[] {
  let text = output.trim().replace(/\\n/g, "\n").replace(/\\"/g, '"');
  const fenced = /```(?:json)?([\s\S]*?)```/.exec(text);
  if (fenced) text = fenced[1].trim();
  const start = text.indexOf("[");
  const end = text.lastIndexOf("]");
  const candidate = start >= 0 && end > start ? text.slice(start, end + 1) : text;

  let parsed: unknown;
  try {
    parsed = JSON.parse(candidate);
  } catch {
    try {
      parsed = JSON.parse(candidate.split("'").join('"'));
    } catch {
      parsed = undefined;
    }
  }
  if (parsed === undefined) {
    throw new Error(`Could not parse restaurants from the task output: ${output.slice(0, 2000)}`);
  }
  if (Array.isArray(parsed)) return parsed as Restaurant[];
  const maybe = parsed as { restaurants?: unknown };
  if (Array.isArray(maybe.restaurants)) return maybe.restaurants as Restaurant[];
  return [parsed as Restaurant];
}

/**
 * Start an OpenTable availability search and return the Browser Use task to poll with
 * `task_status` / `wait_for_task` and read with `search_results`. `query` is a cuisine or
 * restaurant name (empty for everything); `covers` is the party size; `date_time` is
 * `YYYY-MM-DDTHH:MM:SS` in local time, or pass `date` (`YYYY-MM-DD`, default today) and `time`
 * (`HH:MM`, default 19:00). Pick the area with `zipcode`, or `metro` (`sf`, `nyc`, `la`, ...)
 * plus optional `region` key and `neighborhoods` names, or raw `metro_id` / `region_ids` /
 * `neighborhood_ids`; `sort_by` is `rating` (default) or `distance`; `limit` caps the cards the
 * agent extracts (default 10). `profile_id` and `proxy_country_code` override the stored Browser
 * Use profile and US proxy; `max_steps` (default 15) caps agent steps.
 */
export async function start_search(
  args: {
    query?: string;
    covers?: number;
    date_time?: string;
    date?: string;
    time?: string;
    zipcode?: string;
    metro?: string;
    region?: string;
    neighborhoods?: string[];
    metro_id?: number;
    region_ids?: number[];
    neighborhood_ids?: number[];
    sort_by?: string;
    limit?: number;
    max_steps?: number;
    profile_id?: string;
    proxy_country_code?: string;
  },
  ctx: Ctx,
): Promise<StartedTask> {
  let date_time = args.date_time?.trim();
  if (!date_time) {
    const date = args.date?.trim() || todayLocal();
    date_time = `${date}T${(args.time || "19:00").trim()}:00`;
  }
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/.test(date_time)) {
    throw new Error(`date_time must look like 2025-01-31T19:00:00, got ${date_time}`);
  }
  if (!date_time.endsWith(":00") || date_time.length !== 19) date_time = `${date_time}:00`;

  const limit = args.limit ?? 10;
  const location = resolveLocation(args);
  const setup: SearchSetup = {
    url: buildUrl(
      args.query || "",
      args.covers ?? 2,
      date_time,
      location.metro_id,
      location.region_ids,
      location.neighborhood_ids,
      args.sort_by || "rating",
    ),
    metro_id: location.metro_id,
    region_ids: location.region_ids,
    neighborhood_ids: location.neighborhood_ids,
    date_time,
    resolved_from: location.resolved_from,
  };

  const payload = await call(ctx, "/tasks", {
    method: "POST",
    body: {
      task: searchTask(setup.url, limit),
      startUrl: setup.url,
      maxSteps: args.max_steps ?? DEFAULT_MAX_STEPS,
      sessionSettings: {
        profileId: args.profile_id ?? DEFAULT_PROFILE_ID,
        proxyCountryCode: args.proxy_country_code ?? DEFAULT_PROXY_COUNTRY,
      },
    },
  });
  return { task_id: payload.id, session_id: payload.sessionId, setup };
}

/**
 * Poll one task's status: `{ id, status, output, finishedAt, isSuccess }`, where `status` is
 * `created`, `started`, `finished`, `failed` or `stopped` and `output` is the agent's raw text
 * (null until it finishes). Poll every 10–30 s; a search usually takes one to three minutes.
 */
export async function task_status(args: { task_id: string }, ctx: Ctx): Promise<TaskStatus> {
  const id = (args.task_id || "").trim();
  if (!id) throw new Error("task_id is required");
  return call(ctx, `/tasks/${encodeURIComponent(id)}/status`);
}

/**
 * Poll `task_status` until the task leaves `created`/`started` or `timeout_ms` runs out (default
 * 45000, at most 55000 because one call may not exceed 60 s), then return the last status.
 * `status: "started"` means keep polling in a later cell; `poll_interval_ms` defaults to 10000.
 */
export async function wait_for_task(
  args: { task_id: string; timeout_ms?: number; poll_interval_ms?: number },
  ctx: Ctx,
): Promise<TaskStatus> {
  const timeout = Math.min(Math.max(args.timeout_ms ?? 45000, 0), 55000);
  const interval = Math.max(args.poll_interval_ms ?? 10000, 2000);
  const started = Date.now();
  let current = await task_status(args, ctx);
  while (
    (current.status === "created" || current.status === "started") &&
    Date.now() - started + interval <= timeout
  ) {
    await ctx.sleep(interval);
    current = await task_status(args, ctx);
  }
  return current;
}

/**
 * Fetch the restaurants a finished search found, as `{ restaurants }` with `name`, `cuisine`,
 * `neighborhood`, `time_slots` (reservation times as shown), `rating` and `price_range` per
 * card. Throws with the raw output when the task has not finished or its answer is not a JSON
 * array; read it yourself with `task_status` in that case.
 */
export async function search_results(
  args: { task_id: string },
  ctx: Ctx,
): Promise<{ restaurants: Restaurant[]; status: string }> {
  const task = await task_status(args, ctx);
  if (task.status !== "finished") {
    throw new Error(`OpenTable task ${task.id} is ${task.status}, not finished yet`);
  }
  if (!task.output) throw new Error(`OpenTable task ${task.id} finished without output`);
  return { restaurants: parseRestaurants(task.output), status: task.status };
}

/**
 * Stop a running search: `stop` ends the task but keeps its browser session, or
 * `stop_task_and_session` ends both. Returns the updated task. Use it to avoid burning Browser
 * Use credits on a search that is no longer wanted.
 */
export async function stop_task(
  args: { task_id: string; action?: "stop" | "stop_task_and_session" },
  ctx: Ctx,
): Promise<Record<string, any>> {
  const id = (args.task_id || "").trim();
  if (!id) throw new Error("task_id is required");
  return call(ctx, `/tasks/${encodeURIComponent(id)}`, {
    method: "PATCH",
    body: { action: args.action || "stop_task_and_session" },
  });
}

/** Metro areas this connector can resolve by name (`key`), with their OpenTable `id`. */
export async function list_metros(args: {}, ctx: Ctx): Promise<{ metros: Metro[] }> {
  const seen: number[] = [];
  const metros: Metro[] = [];
  for (const key of Object.keys(METROS)) {
    const metro = METROS[key];
    if (seen.includes(metro.id)) continue;
    seen.push(metro.id);
    metros.push({ key, id: metro.id, name: metro.name });
  }
  return { metros };
}

/** Regions of a metro (boroughs, counties and sub-city areas) with their `id`; `all` has no id. */
export async function list_regions(args: { metro: string }, ctx: Ctx): Promise<{ regions: Region[] }> {
  const key = slug(args.metro || "");
  const metro = METROS[key];
  if (!metro) {
    throw new Error(`Unknown metro "${key}"; use one of: ${Object.keys(METROS).join(", ")}`);
  }
  const table = REGIONS[String(metro.id)] ?? {};
  const regions: Region[] = [];
  for (const region_key of Object.keys(table)) {
    regions.push({ key: region_key, id: table[region_key].id, name: table[region_key].name });
  }
  return { regions };
}

/** Neighborhoods mapped for a region (by metro and region key), with their `neighborhoodId`. */
export async function list_neighborhoods(
  args: { metro: string; region: string },
  ctx: Ctx,
): Promise<{ neighborhoods: Neighborhood[] }> {
  const location = resolveRegion(args.metro, args.region);
  const table = NEIGHBORHOODS[String(location)];
  if (!table) {
    throw new Error(
      `No neighborhood IDs are mapped for region ${location}; pass neighborhood_ids to start_search instead`,
    );
  }
  const neighborhoods: Neighborhood[] = [];
  for (const name of Object.keys(table)) neighborhoods.push({ name, id: table[name] });
  return { neighborhoods };
}

/** Zipcodes mapped to OpenTable neighborhoods, optionally one metro's, sorted by zipcode. */
export async function list_zipcodes(
  args: { metro?: string },
  ctx: Ctx,
): Promise<{ zipcodes: ZipcodeEntry[] }> {
  let metro_id: number | undefined;
  if (args.metro) {
    const metro = METROS[slug(args.metro)];
    if (!metro) {
      throw new Error(`Unknown metro "${args.metro}"; use one of: ${Object.keys(METROS).join(", ")}`);
    }
    metro_id = metro.id;
  }
  const zipcodes: ZipcodeEntry[] = [];
  for (const zipcode of Object.keys(ZIPCODES).sort()) {
    const info = ZIPCODES[zipcode];
    if (metro_id !== undefined && info.metro_id !== metro_id) continue;
    zipcodes.push({ zipcode, name: info.name, ids: info.ids, metro_id: info.metro_id, region_id: info.region_id });
  }
  return { zipcodes };
}

/**
 * The metro, region and neighborhood IDs mapped to a zipcode, as `{ zipcode: { name, ids,
 * metro_id, region_id } }`, or `{ zipcode: null }` when the zipcode is not mapped.
 */
export async function zipcode_info(
  args: { zipcode: string },
  ctx: Ctx,
): Promise<{ zipcode: ZipcodeInfo | null }> {
  const zipcode = (args.zipcode || "").trim();
  if (!zipcode) throw new Error("zipcode is required");
  return { zipcode: ZIPCODES[zipcode] ?? null };
}

function resolveRegion(metro_key: string, region_key: string): number | null {
  const metro = METROS[slug(metro_key || "")];
  if (!metro) {
    throw new Error(`Unknown metro "${metro_key}"; use one of: ${Object.keys(METROS).join(", ")}`);
  }
  const regions = REGIONS[String(metro.id)] ?? {};
  const region = regions[slug(region_key || "")];
  if (!region) {
    throw new Error(
      `Unknown region "${region_key}" for metro ${metro.id}; use one of: ${Object.keys(regions).join(", ")}`,
    );
  }
  return region.id;
}
