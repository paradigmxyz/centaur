import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are Listen Notes' own objects; the interfaces name the fields worth
// knowing and Listen Notes sends more.

const DEFAULT_BASE = "https://listen-api.listennotes.com/api/v2";

/** A podcast's metadata, as embedded in episodes and returned by search. */
export interface Podcast {
  id: string;
  title?: string;
  title_original?: string;
  publisher?: string;
  publisher_original?: string;
  description?: string;
  image?: string;
  thumbnail?: string;
  listennotes_url?: string;
  total_episodes?: number;
  audio_length_sec?: number;
  explicit_content?: boolean;
  latest_episode_id?: string;
  latest_pub_date_ms?: number;
  language?: string;
  country?: string;
  genre_ids?: number[];
  [field: string]: unknown;
}

/** An episode's metadata, including audio/transcript URLs and its podcast. */
export interface Episode {
  id: string;
  title?: string;
  title_original?: string;
  description?: string;
  audio?: string;
  audio_length_sec?: number;
  listennotes_url?: string;
  transcript?: string;
  pub_date_ms?: number;
  maybe_audio_invalid?: boolean;
  podcast?: Podcast;
  [field: string]: unknown;
}

/** A `search` response. `results` holds episodes or podcasts according to `type`. */
export interface SearchResponse {
  took?: number;
  count?: number;
  total?: number;
  next_offset?: number;
  results?: (Episode | Podcast)[];
  [field: string]: unknown;
}

async function request(ctx: Ctx, path: string, params?: Params): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.LISTENNOTES_KEY) headers["X-ListenAPI-Key"] = ctx.env.LISTENNOTES_KEY;
  const res = await ctx.fetch((ctx.env.LISTENNOTES_API_URL || DEFAULT_BASE) + path + query(params ?? {}), { headers });
  return readJson(res, "Listen Notes");
}

/**
 * Search episodes or podcasts. `type` is "episode" (default) or "podcast". `len_min` and `len_max`
 * bound audio length in minutes. Page by passing the previous response's `next_offset` as `offset`
 * (there is no page-count limit, but Listen Notes caps how deep a search can go); read `results`.
 */
export async function search(
  args: { query: string; type?: "episode" | "podcast"; offset?: number; len_min?: number; len_max?: number },
  ctx: Ctx,
): Promise<SearchResponse> {
  const params: Params = { q: args.query, type: args.type ?? "episode", offset: args.offset ?? 0 };
  if (args.len_min !== undefined) params.len_min = args.len_min;
  if (args.len_max !== undefined) params.len_max = args.len_max;
  return request(ctx, "/search", params);
}

/**
 * One podcast's full metadata by `podcast_id` (from search): title, publisher, description,
 * image, episode count, language, country, genres and latest episode.
 */
export async function get_podcast(args: { podcast_id: string }, ctx: Ctx): Promise<Podcast> {
  return request(ctx, `/podcasts/${encodeURIComponent(args.podcast_id)}`);
}

/**
 * One episode's full metadata by `episode_id` (from search): title, description, audio URL,
 * transcript URL, duration in seconds, publication time in epoch milliseconds and podcast.
 */
export async function get_episode(args: { episode_id: string }, ctx: Ctx): Promise<Episode> {
  return request(ctx, `/episodes/${encodeURIComponent(args.episode_id)}`);
}
