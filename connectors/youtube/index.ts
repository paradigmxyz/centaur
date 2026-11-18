import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are YouTube's own objects, passed through. The Data API calls return YouTube's
// list envelope (`kind`, `etag`, `nextPageToken`, `pageInfo`, `items`); single lookups unwrap
// `items[0]`. Transcript functions scrape the public watch page and caption tracks because the
// Data API does not expose public captions.

const YOUTUBE_API = "https://www.googleapis.com/youtube/v3";
const WATCH_URL = "https://www.youtube.com/watch?v=";
const INNERTUBE_API_URL = "https://www.youtube.com/youtubei/v1/player";
const INNERTUBE_CONTEXT = { client: { clientName: "ANDROID", clientVersion: "20.10.38" } };
const VIDEO_ID_RE = /^[A-Za-z0-9_-]{11}$/;
const INNERTUBE_API_KEY_RE = /"INNERTUBE_API_KEY":\s*"([A-Za-z0-9_-]+)"/;
const TAG_RE = /<[^>]+>/g;

/** A YouTube thumbnail at one size (`default`, `medium`, `high`, `standard`, `maxres`). */
export interface Thumbnail {
  url: string;
  width?: number;
  height?: number;
  [field: string]: unknown;
}

/** The `snippet` shared by search results, videos and channels. Text is usually English. */
export interface Snippet {
  publishedAt: string;
  channelId: string;
  title: string;
  description: string;
  thumbnails: Record<string, Thumbnail>;
  channelTitle?: string;
  tags?: string[];
  categoryId?: string;
  liveBroadcastContent?: string;
  defaultAudioLanguage?: string;
  defaultLanguage?: string;
  localized?: { title: string; description: string };
  [field: string]: unknown;
}

/** One `/search` item. Exactly one of `videoId`, `channelId` or `playlistId` is set. */
export interface SearchResultItem {
  kind: string;
  etag: string;
  id: { kind: string; videoId?: string; channelId?: string; playlistId?: string };
  snippet: Snippet;
  [field: string]: unknown;
}

/** YouTube's `/search` list envelope. Follow `nextPageToken` as `page_token` for more. */
export interface SearchListResponse {
  kind: string;
  etag: string;
  nextPageToken?: string;
  prevPageToken?: string;
  regionCode?: string;
  pageInfo: { totalResults: number; resultsPerPage: number };
  items: SearchResultItem[];
  [field: string]: unknown;
}

/** A `/videos` item: snippet, contentDetails (ISO 8601 `duration`) and statistics (string counts). */
export interface VideoItem {
  kind: string;
  etag: string;
  id: string;
  snippet: Snippet;
  contentDetails?: {
    duration?: string;
    dimension?: string;
    definition?: string;
    caption?: string;
    licensedContent?: boolean;
    regionRestriction?: Record<string, unknown>;
    [field: string]: unknown;
  };
  statistics?: {
    viewCount?: string;
    likeCount?: string;
    favoriteCount?: string;
    commentCount?: string;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** A `/channels` item. Statistics counts are strings; `hiddenSubscriberCount` hides subscribers. */
export interface ChannelItem {
  kind: string;
  etag: string;
  id: string;
  snippet: Snippet;
  statistics?: {
    viewCount?: string;
    subscriberCount?: string;
    hiddenSubscriberCount?: boolean;
    videoCount?: string;
    [field: string]: unknown;
  };
  brandingSettings?: Record<string, unknown>;
  contentDetails?: Record<string, unknown>;
  [field: string]: unknown;
}

/** One public caption track. `language_code` is an ISO code such as `en` or `en-US`. */
export interface CaptionTrack {
  language: string;
  language_code: string | null;
  is_generated: boolean;
  is_translatable: boolean;
}

/** One caption cue. `start`, `duration` and `end` are seconds from the video start. */
export interface TranscriptSegment {
  text: string;
  start: number;
  duration: number;
  end: number;
}

export interface TranscriptList {
  video_id: string;
  tracks: CaptionTrack[];
}

export interface Transcript {
  video_id: string;
  language: string;
  language_code: string | null;
  is_generated: boolean;
  /** Requested window in seconds, or null when unbounded. Negative offsets were resolved. */
  window_start: number | null;
  window_end: number | null;
  /** Start of the first returned cue and end of the last one. */
  transcript_start: number;
  transcript_end: number;
  transcript: TranscriptSegment[];
  /** The returned cues joined with single spaces. */
  text: string;
}

interface PlayerResponse {
  playabilityStatus?: { status?: string; reason?: string };
  captions?: {
    playerCaptionsTracklistRenderer?: { captionTracks?: CaptionTrackResource[] };
  };
  [field: string]: unknown;
}

interface CaptionTrackResource {
  baseUrl?: string;
  languageCode?: string;
  kind?: string;
  isTranslatable?: boolean;
  name?: { simpleText?: string; runs?: { text?: string }[] };
  [field: string]: unknown;
}

// YouTube serves the watch page and captions without credentials; a browser-like UA keeps it
// from treating the runner as a bot.
function transcriptHeaders(): Record<string, string> {
  return {
    "accept-language": "en-US",
    "user-agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36",
  };
}

function apiKey(ctx: Ctx): string | undefined {
  return ctx.env.YOUTUBE_API_KEY || ctx.env.GOOGLE_API_KEY;
}

async function youtubeRequest(ctx: Ctx, endpoint: string, params: Params): Promise<any> {
  // Behind iron-proxy the key is injected as the `key` query parameter on the wire; locally it
  // comes from the env. Sending no key lets the proxy supply one.
  const paramsWithKey: Params = { ...params, key: apiKey(ctx) };
  const res = await ctx.fetch(`${YOUTUBE_API}${endpoint}${query(paramsWithKey)}`, {
    headers: { accept: "application/json", "accept-language": "en-US" },
  });
  return readJson(res, `YouTube Data API ${endpoint}`);
}

function decodeEntity(entity: string): string {
  if (entity[0] === "#") {
    const code = entity[1] === "x" || entity[1] === "X"
      ? parseInt(entity.slice(2), 16)
      : parseInt(entity.slice(1), 10);
    if (Number.isNaN(code) || code < 0 || code > 0x10ffff) return `&${entity};`;
    return String.fromCodePoint(code);
  }
  const named: Record<string, string> = {
    amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: "\u00a0",
  };
  return named[entity] ?? `&${entity};`;
}

/** Minimal HTML entity decoding (named entities plus numeric references). */
function unescapeHtml(text: string): string {
  return text.replace(/&(#x?[0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);/g, (_match, entity: string) =>
    decodeEntity(entity),
  );
}

/**
 * Accept a bare 11-character video ID or a common YouTube URL (`youtu.be/…`,
 * `/watch?v=…`, `/embed/…`, `/live/…`, `/shorts/…`) and return the ID.
 */
function normalizeVideoId(videoIdOrUrl: string): string {
  const raw = videoIdOrUrl.trim();
  if (VIDEO_ID_RE.test(raw)) return raw;
  const candidate = /^[a-z][a-z0-9+.-]*:\/\//i.test(raw) || raw.startsWith("//")
    ? raw
    : /^(www\.|m\.)?(youtube\.com|youtu\.be)(\/|$)/i.test(raw)
      ? `https://${raw}`
      : raw;
  const parsed = /^(?:[a-z][a-z0-9+.-]*:)?\/\/([^/?#]+)([^?#]*)(?:\?([^#]*))?/i.exec(candidate);
  let id = "";
  if (parsed) {
    const host = parsed[1].toLowerCase().replace(/^.*@/, "").replace(/:\d+$/, "");
    const path = parsed[2] ?? "";
    const parts = path.split("/").filter(Boolean);
    if (host === "youtu.be" && parts.length) {
      id = parts[0];
    } else if (host.endsWith("youtube.com")) {
      if (path === "/watch") {
        id = searchParam(parsed[3] ?? "", "v");
      } else if (parts.length >= 2 && ["embed", "live", "shorts", "v"].includes(parts[0])) {
        id = parts[1];
      }
    }
  }
  if (VIDEO_ID_RE.test(id)) return id;
  throw new Error(`Invalid YouTube video ID or URL: ${videoIdOrUrl}`);
}

function searchParam(queryString: string, name: string): string {
  for (const pair of queryString.split("&")) {
    if (!pair) continue;
    const index = pair.indexOf("=");
    const key = index < 0 ? pair : pair.slice(0, index);
    if (decodeURIComponent(key.replace(/\+/g, " ")) === name) {
      return decodeURIComponent((index < 0 ? "" : pair.slice(index + 1)).replace(/\+/g, " "));
    }
  }
  return "";
}

async function fetchWatchHtml(ctx: Ctx, videoId: string): Promise<string> {
  const url = `${WATCH_URL}${videoId}`;
  let res = await ctx.fetch(url, { headers: transcriptHeaders() });
  if (!res.ok) throw new Error(`YouTube transcript metadata request failed: ${res.status}`);
  let html = unescapeHtml(await res.text());
  if (!html.includes('action="https://consent.youtube.com/s"')) return html;

  const consent = /name="v" value="(.*?)"/.exec(html);
  if (!consent) throw new Error("YouTube transcript retrieval was blocked by a consent wall.");
  const headers = transcriptHeaders();
  headers.cookie = `CONSENT=YES+${consent[1]}`;
  res = await ctx.fetch(url, { headers });
  if (!res.ok) throw new Error(`YouTube transcript metadata request failed: ${res.status}`);
  html = unescapeHtml(await res.text());
  if (html.includes('action="https://consent.youtube.com/s"')) {
    throw new Error("YouTube transcript retrieval was blocked by a consent wall.");
  }
  return html;
}

async function fetchPlayerResponse(ctx: Ctx, videoId: string): Promise<PlayerResponse> {
  const html = await fetchWatchHtml(ctx, videoId);
  const match = INNERTUBE_API_KEY_RE.exec(html);
  if (!match) throw new Error("Could not resolve YouTube transcript metadata.");
  const res = await ctx.fetch(`${INNERTUBE_API_URL}${query({ key: match[1] })}`, {
    method: "POST",
    headers: { ...transcriptHeaders(), "content-type": "application/json" },
    body: JSON.stringify({ context: INNERTUBE_CONTEXT, videoId }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`YouTube transcript discovery failed: ${res.status} - ${text.slice(0, 1000)}`);
  return JSON.parse(text);
}

async function fetchCaptionTracks(ctx: Ctx, videoId: string): Promise<CaptionTrackResource[]> {
  const data = await fetchPlayerResponse(ctx, videoId);
  const playability = data.playabilityStatus ?? {};
  if (playability.status && playability.status !== "OK") {
    throw new Error(`YouTube transcript unavailable: ${playability.reason || playability.status}`);
  }
  const tracks = data.captions?.playerCaptionsTracklistRenderer?.captionTracks ?? [];
  if (!tracks.length) throw new Error("No public captions are available for this video.");
  return tracks;
}

function trackName(track: CaptionTrackResource): string {
  const name = track.name ?? {};
  if (typeof name.simpleText === "string") return name.simpleText;
  if (Array.isArray(name.runs)) return name.runs.map((run) => run.text ?? "").join("");
  return track.languageCode ?? "unknown";
}

function languageMatches(trackCode: string, requestedCode: string): boolean {
  const track = trackCode.toLowerCase();
  const requested = requestedCode.toLowerCase();
  if (track === requested) return true;
  return track.split("-")[0] === requested.split("-")[0];
}

/** Prefer manual over generated tracks, and the requested language order over defaults. */
function selectCaptionTrack(
  tracks: CaptionTrackResource[],
  languageCodes: string[] | undefined,
): CaptionTrackResource {
  const manual = tracks.filter((track) => track.kind !== "asr");
  const generated = tracks.filter((track) => track.kind === "asr");
  if (languageCodes && languageCodes.length) {
    for (const code of languageCodes) {
      for (const pool of [manual, generated]) {
        for (const track of pool) {
          if (languageMatches(track.languageCode ?? "", code)) return track;
        }
      }
    }
    const available = tracks.map((track) => track.languageCode ?? "").join(", ");
    throw new Error(
      `No public captions matched the requested languages: ${languageCodes.join(", ")}. ` +
        `Available languages: ${available || "none"}.`,
    );
  }
  for (const pool of [manual, generated]) {
    for (const track of pool) {
      if (languageMatches(track.languageCode ?? "", "en")) return track;
    }
  }
  return manual.length ? manual[0] : generated[0];
}

/** Seconds or `HH:MM:SS`/`MM:SS` (optionally signed) to seconds. */
function parseOffset(value: number | string | null | undefined): number | null {
  if (value === null || value === undefined) return null;
  if (typeof value === "number") return value;
  let text = value.trim();
  if (!text) return null;
  let sign = 1;
  if (text[0] === "+" || text[0] === "-") {
    if (text[0] === "-") sign = -1;
    text = text.slice(1);
  }
  if (!text.includes(":")) {
    const seconds = Number(text);
    if (Number.isNaN(seconds)) throw new Error(`Invalid transcript offset: ${value}`);
    return sign * seconds;
  }
  const parts = text.split(":");
  if (parts.length > 3 || parts.some((part) => !/^\d+$/.test(part))) {
    throw new Error(`Invalid transcript offset: ${value}`);
  }
  let seconds = 0;
  for (const part of parts) seconds = seconds * 60 + Number(part);
  return sign * seconds;
}

function numberAttribute(attributes: string, name: string): number {
  const match = new RegExp(`(?:^|\\s)${name}="([^"]*)"`).exec(attributes);
  if (!match || !match[1]) return 0;
  const value = Number(match[1]);
  return Number.isNaN(value) ? 0 : value;
}

/** Parse YouTube's timedtext XML (`<text start="…" dur="…">…</text>`) without a DOM. */
function parseTranscriptXml(xml: string): TranscriptSegment[] {
  const segments: TranscriptSegment[] = [];
  const pattern = /<text\b([^>]*?)(?:\/>|>([\s\S]*?)<\/text>)/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(xml)) !== null) {
    const attributes = match[1];
    const content = match[2] ?? "";
    const text = unescapeHtml(content.replace(/<!\[CDATA\[([\s\S]*?)\]\]>/g, "$1"))
      .replace(TAG_RE, "")
      .trim();
    if (!text) continue;
    const start = numberAttribute(attributes, "start");
    const duration = numberAttribute(attributes, "dur");
    segments.push({ text, start, duration, end: start + duration });
  }
  if (!segments.length) throw new Error("No public captions are available for this video.");
  return segments;
}

function normalizeWindow(
  startTime: number | null,
  endTime: number | null,
  transcriptEnd: number,
): { start: number | null; end: number | null } {
  let start = startTime !== null && startTime < 0 ? transcriptEnd + startTime : startTime;
  let end = endTime !== null && endTime < 0 ? transcriptEnd + endTime : endTime;
  if (start !== null) start = Math.max(0, start);
  if (end !== null) end = Math.max(0, end);
  if (start !== null && end !== null && end < start) {
    throw new Error("Transcript end offset must be greater than or equal to the start offset.");
  }
  return { start, end };
}

/**
 * Search videos, channels or playlists. Returns YouTube's `/search` list envelope: `items`
 * (each with `id.videoId`/`channelId`/`playlistId` and `snippet`), `pageInfo`, and
 * `nextPageToken`; pass that token back as `page_token` for the next page.
 * `limit` is 1–50 (default 10); `type` defaults to `video`; `order` is `relevance` (default),
 * `date`, `rating`, `title`, `videoCount` or `viewCount`.
 */
export async function search(
  args: {
    query: string;
    limit?: number;
    type?: "video" | "channel" | "playlist";
    order?: "date" | "rating" | "relevance" | "title" | "videoCount" | "viewCount";
    page_token?: string;
  },
  ctx: Ctx,
): Promise<SearchListResponse> {
  const limit = Math.min(Math.max(args.limit ?? 10, 1), 50);
  const data = await youtubeRequest(ctx, "/search", {
    part: "snippet",
    q: args.query,
    maxResults: limit,
    type: args.type ?? "video",
    order: args.order ?? "relevance",
    pageToken: args.page_token,
  });
  return data as SearchListResponse;
}

/**
 * One video by 11-character ID: snippet (title, description, channel, tags), contentDetails
 * (`duration` as ISO 8601, e.g. `PT1H2M3S`) and statistics (view/like/comment counts as strings).
 * Throws when the ID does not exist.
 */
export async function get_video(args: { video_id: string }, ctx: Ctx): Promise<{ video: VideoItem }> {
  const data = await youtubeRequest(ctx, "/videos", {
    part: "snippet,contentDetails,statistics",
    id: args.video_id,
  });
  const items = (data.items ?? []) as VideoItem[];
  if (!items.length) throw new Error(`Video not found: ${args.video_id}`);
  return { video: items[0] };
}

/**
 * One channel by channel ID (`UC…`): snippet (title, handle, description, creation date) and
 * statistics (subscriber, view and video counts as strings; subscribers may be hidden).
 * Throws when the ID does not exist.
 */
export async function get_channel(args: { channel_id: string }, ctx: Ctx): Promise<{ channel: ChannelItem }> {
  const data = await youtubeRequest(ctx, "/channels", {
    part: "snippet,statistics",
    id: args.channel_id,
  });
  const items = (data.items ?? []) as ChannelItem[];
  if (!items.length) throw new Error(`Channel not found: ${args.channel_id}`);
  return { channel: items[0] };
}

/**
 * List the public caption tracks for a video, given its 11-character ID or a YouTube URL.
 * `is_generated` marks auto-generated (ASR) tracks; `is_translatable` marks tracks YouTube can
 * translate. Throws when the video has no public captions or is not playable.
 */
export async function list_transcripts(args: { video_id: string }, ctx: Ctx): Promise<TranscriptList> {
  const videoId = normalizeVideoId(args.video_id);
  const tracks = await fetchCaptionTracks(ctx, videoId);
  return {
    video_id: videoId,
    tracks: tracks.map((track) => ({
      language: trackName(track),
      language_code: track.languageCode ?? null,
      is_generated: track.kind === "asr",
      is_translatable: Boolean(track.isTranslatable),
    })),
  };
}

/**
 * Download a timestamped public transcript, given a video ID or YouTube URL.
 *
 * `language_codes` is a preference list of ISO codes (`en`, `en-US`, `de`); the first matching
 * track wins, manual before auto-generated. Without it, English (or the first manual track, then
 * the first generated one) is used. `start_time`/`end_time` accept seconds (`90`) or
 * `HH:MM:SS`/`MM:SS`; negative offsets count back from the end of the transcript, and a negative
 * end is relative to the end too. Only cues overlapping the window are returned, with `start`,
 * `duration`, `end` in seconds, plus the joined `text`. Throws when no captions match or nothing
 * overlaps the window.
 */
export async function get_transcript(
  args: {
    video_id: string;
    language_codes?: string[];
    start_time?: number | string;
    end_time?: number | string;
  },
  ctx: Ctx,
): Promise<Transcript> {
  const videoId = normalizeVideoId(args.video_id);
  const tracks = await fetchCaptionTracks(ctx, videoId);
  const track = selectCaptionTrack(tracks, args.language_codes);

  const transcriptUrl = (track.baseUrl ?? "").replace("&fmt=srv3", "");
  if (!transcriptUrl) throw new Error("No public captions are available for this video.");
  if (transcriptUrl.includes("&exp=xpe")) {
    throw new Error("YouTube requires extra authorization for this video's public captions.");
  }
  const res = await ctx.fetch(transcriptUrl, { headers: transcriptHeaders() });
  if (!res.ok) throw new Error(`YouTube transcript download failed: ${res.status} - ${(await res.text()).slice(0, 1000)}`);

  const transcript = parseTranscriptXml(await res.text());
  const transcriptEnd = transcript.reduce((max, segment) => Math.max(max, segment.end), 0);
  const window = normalizeWindow(
    parseOffset(args.start_time),
    parseOffset(args.end_time),
    transcriptEnd,
  );

  const sliced = transcript.filter(
    (segment) =>
      (window.start === null || segment.end > window.start) &&
      (window.end === null || segment.start < window.end),
  );
  if (!sliced.length) throw new Error("No transcript entries overlap the requested time range.");

  return {
    video_id: videoId,
    language: trackName(track),
    language_code: track.languageCode ?? null,
    is_generated: track.kind === "asr",
    window_start: window.start,
    window_end: window.end,
    transcript_start: sliced[0].start,
    transcript_end: sliced[sliced.length - 1].end,
    transcript: sliced,
    text: sliced.map((segment) => segment.text).join(" "),
  };
}
