import type { Ctx } from "../centaur";
import { downloadTo, outputPath, requireFiles, safeFilename } from "../_lib/files";

// A Firefox Profiler profile is one JSON document: `meta`, `threads` (each with
// samples, markers, stack/frame/func tables and a string table) and `shared`.
// These helpers download it, resolve stacks and summarize; the profile itself is
// never rewritten.

const STORAGE_BASE = "https://storage.googleapis.com/profile-store";
const ALLOWED_HOSTS = ["share.firefox.dev", "profiler.firefox.com", "storage.googleapis.com"];
/** Downloads and decompressed profiles are capped at 10 MiB by the runtime. */
const SIZE_NOTE = "profiles over ~10 MiB (decompressed) cannot be read here";

/** A profile saved to a file. */
export interface SavedFile {
  /** Absolute path in the sandbox. */
  path: string;
  bytes: number;
  mime_type?: string;
}

/**
 * Where a profile comes from: a share URL, or a file in the sandbox such as one `fetch_profile`
 * saved. Pass exactly one of `url` and `path`.
 */
export interface ProfileSource {
  /**
   * Share or download URL. Accepted hosts: share.firefox.dev, profiler.firefox.com and
   * storage.googleapis.com.
   */
  url?: string;
  /** A profile file: plain JSON, or gzip compressed (`.json`, `.json.gz`; detected by content). */
  path?: string;
}

/** Profile metadata (`meta`). `interval` is the sampling interval in ms. */
export interface ProfileMeta {
  interval: number;
  startTime?: number;
  product?: string;
  platform?: string;
  version?: number;
  /** Category index → name and color; marker and sample categories index into this. */
  categories?: { name?: string; color?: string; subcategories?: string[] }[];
  [field: string]: unknown;
}

/** The per-thread sample table. `time` may be absent (older profiles only have `timeDeltas`). */
export interface SamplesTable {
  length?: number;
  /** Stack table index per sample; null for samples with no stack (idle). */
  stack?: (number | null)[];
  /** Sample times in ms, relative to the profile start. */
  time?: number[];
  /** Time since the previous sample in ms, when `time` is absent. */
  timeDeltas?: number[];
  weight?: (number | null)[];
  weightType?: string;
  /** CPU time in ns since the previous sample, where the platform provides it. */
  threadCPUDelta?: (number | null)[];
  [field: string]: unknown;
}

/** The per-thread marker (event) table. */
export interface MarkersTable {
  length?: number;
  /** String table index per marker. */
  name?: (number | null)[];
  startTime?: (number | null)[];
  endTime?: (number | null)[];
  /** 0 instant, 1 interval, 2 interval start, 3 interval end. */
  phase?: number[];
  category?: number[];
  data?: unknown[];
  [field: string]: unknown;
}

/** One thread of the profile. */
export interface ProfileThread {
  tid: string | number;
  pid?: string | number;
  name?: string;
  processName?: string;
  isMainThread?: boolean;
  samples?: SamplesTable;
  markers?: MarkersTable;
  stackTable?: { frame: (number | null)[]; prefix: (number | null)[] | null; [field: string]: unknown };
  frameTable?: { func: number[]; [field: string]: unknown };
  funcTable?: { name: number[]; [field: string]: unknown };
  /** Thread-local strings in older profiles; processed profiles use `stringArray`. */
  stringTable?: string[];
  /** Thread-local strings in processed profiles (`funcTable.name`, `markers.name` index into it). */
  stringArray?: string[];
  [field: string]: unknown;
}

/** A complete Firefox Profiler profile. */
export interface FirefoxProfile {
  meta: ProfileMeta;
  threads: ProfileThread[];
  shared?: { stringArray?: string[]; [field: string]: unknown };
  [field: string]: unknown;
}

/** Summary statistics for one thread; times are ms from the profile start. */
export interface ThreadSummary {
  tid: string | number;
  name: string;
  pid: string | number;
  process_name: string;
  is_main_thread: boolean;
  sample_count: number;
  marker_count: number;
  start_time_ms: number;
  end_time_ms: number;
  duration_ms: number;
  /** Sum of `threadCPUDelta`, in ns. */
  cpu_delta_total_ns: number;
}

/** One sample with its resolved stack. */
export interface Sample {
  time_ms: number;
  /** Function names, leaf first, then callers up to the root. */
  stack: string[];
  weight: number;
  cpu_delta_ns?: number | null;
  thread_id?: string | number;
}

/** One marker (event or interval) in a thread. */
export interface Marker {
  name: string;
  category: string;
  start_ms: number;
  end_ms?: number;
  duration_ms?: number;
  /** instant, interval, start or end. Omitted for instant markers. */
  phase?: string;
  data?: Record<string, unknown>;
  thread_id?: string | number;
}

/** One hot function or stack. Percentages are of all samples in the thread and range. */
export interface Hotspot {
  name: string;
  self_samples: number;
  self_weight: number;
  total_samples: number;
  total_weight: number;
  self_pct: number;
  total_pct: number;
}

/** One time bucket of `timeline`; times are ms and follow the profile's own clock. */
export interface TimelineBucket {
  start_ms: number;
  end_ms: number;
  sample_count: number;
  total_weight: number;
  cpu_delta_ns: number;
  /** Up to three most sampled leaf functions in the bucket. */
  top_functions: string[];
}

/** The host of an http(s) URL, lowercased and without its port. */
function hostOf(url: string): string {
  const match = /^https?:\/\/([^/?#]+)/i.exec(url.trim());
  if (!match) throw new Error(`not an http(s) URL: ${JSON.stringify(url)}`);
  return match[1].toLowerCase().replace(/:\d+$/, "");
}

/** Map a profiler or share URL to the URL the profile is downloaded from. */
function downloadUrl(url: string): string {
  const trimmed = url.trim();
  const host = hostOf(trimmed);
  if (!ALLOWED_HOSTS.includes(host)) {
    throw new Error(`Host '${host}' not allowed. Expected one of: ${ALLOWED_HOSTS.join(", ")}`);
  }
  if (host === "profiler.firefox.com") {
    const match = /\/public\/([^/?#]+)/.exec(trimmed);
    if (!match) throw new Error(`Could not extract profile token from URL: ${trimmed}`);
    return `${STORAGE_BASE}/${match[1]}`;
  }
  if (host === "storage.googleapis.com") {
    const path = trimmed.slice(trimmed.indexOf(host) + host.length).split(/[?#]/)[0];
    if (!path.startsWith("/profile-store/")) {
      throw new Error(`Storage URL must use /profile-store/ path: ${trimmed}`);
    }
  }
  return trimmed;
}

function isGzip(bytes: Uint8Array): boolean {
  return bytes.length > 1 && bytes[0] === 0x1f && bytes[1] === 0x8b;
}

/**
 * Parse a profile document from `source` (a URL or path, for error messages); gzipped payloads
 * are decompressed with `ctx.gunzip`.
 */
function parseProfile(ctx: Ctx, bytes: Uint8Array, source: string): FirefoxProfile {
  // `ctx.fetch` already decodes a gzip Content-Encoding; profiles are often
  // also stored as a gzip file, which shows up as the magic bytes.
  const data = isGzip(bytes) ? ctx.gunzip(bytes) : bytes;
  // No TextDecoder here: round-trip through the host's UTF-8 codec.
  const text = ctx.base64.decodeText(ctx.base64.encode(data));
  try {
    const profile = JSON.parse(text) as FirefoxProfile;
    if (!profile || typeof profile !== "object" || !Array.isArray(profile.threads)) {
      throw new Error("no threads");
    }
    return profile;
  } catch {
    throw new Error(
      `${source} did not contain a Firefox Profiler profile (JSON with a threads array, plain or ` +
        `gzip compressed; an HTML page or a truncated body looks the same); short share links only ` +
        `work when they redirect straight to a downloadable profile. ${SIZE_NOTE}`,
    );
  }
}

/** Load a profile from a `url` or a saved file `path`; `what` names the call in file errors. */
async function loadProfile(ctx: Ctx, args: ProfileSource, what: string): Promise<FirefoxProfile> {
  const url = (args.url ?? "").trim();
  const path = (args.path ?? "").trim();
  if (url && path) throw new Error("pass url or path, not both");
  if (path) {
    const files = requireFiles(ctx, what);
    if (!(await files.stat(path))) throw new Error(`no such file: ${path}`);
    return parseProfile(ctx, await files.read(path), path);
  }
  if (!url) throw new Error("url (a share link) or path (a saved profile file) is required");
  const source = downloadUrl(url);
  const res = await ctx.fetch(source, { headers: { "user-agent": "profslice/0.1.0" } });
  if (!res.ok) {
    throw new Error(`profile download returned HTTP ${res.status}: ${(await res.text()).slice(0, 300)}`);
  }
  return parseProfile(ctx, await res.bytes(), source);
}

/** `<storage id>.json` for a download URL; the analysis functions detect gzip by content. */
function profileFilename(url: string): string {
  const path = url.split(/[?#]/)[0];
  const segment = path.slice(path.lastIndexOf("/") + 1).replace(/:download$/, "");
  const stem = segment.replace(/\.gz$/i, "").replace(/\.json$/i, "");
  return `${safeFilename(stem || "profile")}.json`;
}

function categoryMap(meta: ProfileMeta): Map<number, string> {
  const map = new Map<number, string>();
  (meta.categories ?? []).forEach((category, index) => map.set(index, category.name ?? `Category${index}`));
  return map;
}

function categoryName(map: Map<number, string>, index: number | null | undefined): string {
  if (index === null || index === undefined) return "Other";
  return map.get(index) ?? "Other";
}

/**
 * The strings a thread's `funcTable.name` and `markers.name` indexes point into: processed
 * profiles keep them in `thread.stringArray`, older ones in `thread.stringTable` or the shared
 * `shared.stringArray`.
 */
function stringTable(profile: FirefoxProfile, thread: ProfileThread): string[] {
  const local = thread.stringArray ?? thread.stringTable ?? [];
  return local.length ? local : (profile.shared?.stringArray ?? []);
}

/** Sample times in ms: `time`, or reconstructed from `timeDeltas`. */
function sampleTimes(samples: SamplesTable): number[] {
  if (samples.time && samples.time.length) return samples.time;
  const deltas = samples.timeDeltas ?? [];
  let time = 0;
  return deltas.map((delta) => (time += delta || 0));
}

function findThread(profile: FirefoxProfile, filter?: string | number): ProfileThread | null {
  if (filter === undefined || filter === null || filter === "") {
    return profile.threads.find((thread) => thread.isMainThread) ?? profile.threads[0] ?? null;
  }
  const wanted = String(filter);
  const exact = profile.threads.find((thread) => String(thread.tid) === wanted);
  if (exact) return exact;
  const pattern = wanted.toLowerCase();
  return profile.threads.find((thread) => (thread.name ?? "").toLowerCase().includes(pattern)) ?? null;
}

/** Resolve a stack index to function names, leaf first. */
function resolveStack(
  profile: FirefoxProfile,
  thread: ProfileThread,
  stackIndex: number | null | undefined,
  maxDepth: number,
): string[] {
  if (stackIndex === null || stackIndex === undefined) return [];
  const frames = thread.stackTable?.frame ?? [];
  const prefixes = thread.stackTable?.prefix ?? [];
  const funcIndices = thread.frameTable?.func ?? [];
  const funcNames = thread.funcTable?.name ?? [];
  const strings = stringTable(profile, thread);
  const out: string[] = [];
  let current: number | null | undefined = stackIndex;
  let depth = 0;
  while (current !== null && current !== undefined && depth < maxDepth) {
    if (current >= frames.length) break;
    const frame = frames[current];
    if (frame !== null && frame !== undefined) {
      const func = frame < funcIndices.length ? funcIndices[frame] : null;
      if (func !== null && func !== undefined) {
        const name = func < funcNames.length ? funcNames[func] : null;
        out.push(name !== null && name !== undefined && name < strings.length ? strings[name] : `<func:${func}>`);
      } else {
        out.push(`<frame:${frame}>`);
      }
    }
    current = prefixes && current < prefixes.length ? prefixes[current] : null;
    depth += 1;
  }
  return out;
}

/** Parse `1s`, `500ms`, `2m` or a bare number of ms. */
function parseMillis(value: string, what: string): number {
  const text = value.trim();
  const amount = text.endsWith("ms")
    ? Number(text.slice(0, -2))
    : text.endsWith("s")
      ? Number(text.slice(0, -1)) * 1000
      : text.endsWith("m")
        ? Number(text.slice(0, -1)) * 60 * 1000
        : Number(text);
  if (!Number.isFinite(amount)) throw new Error(`invalid ${what}: ${JSON.stringify(value)}`);
  return amount;
}

/**
 * Parse a range such as `1000-2000` (ms), `1s-2s` or `1s+500ms` (start plus duration) into
 * `[start_ms, end_ms]`. `undefined` means the whole profile.
 */
function parseTimeRange(range: string | undefined): [number, number] | null {
  if (range === undefined || range === "") return null;
  const text = range.trim();
  const plus = text.indexOf("+");
  if (plus > 0) {
    const start = parseMillis(text.slice(0, plus), "time range");
    const duration = parseMillis(text.slice(plus + 1), "time range");
    return [start, start + duration];
  }
  const dash = /^([^-]+)-(.+)$/.exec(text);
  if (!dash) throw new Error(`invalid time range: ${JSON.stringify(range)}`);
  return [parseMillis(dash[1], "time range"), parseMillis(dash[2], "time range")];
}

function getSamples(
  profile: FirefoxProfile,
  thread: ProfileThread,
  range: [number, number] | null,
  maxDepth: number,
): Sample[] {
  const table = thread.samples ?? {};
  const stacks = table.stack ?? [];
  const times = sampleTimes(table);
  const weights = table.weight ?? [];
  const cpuDeltas = table.threadCPUDelta ?? [];
  const out: Sample[] = [];
  const count = Math.min(stacks.length, times.length);
  for (let index = 0; index < count; index += 1) {
    const time = times[index];
    if (range && (time < range[0] || time > range[1])) continue;
    const weight = weights[index];
    out.push({
      time_ms: Math.round(time * 1000) / 1000,
      stack: resolveStack(profile, thread, stacks[index], maxDepth),
      weight: weight || 1,
      cpu_delta_ns: cpuDeltas.length ? cpuDeltas[index] : null,
      thread_id: thread.tid,
    });
  }
  return out;
}

function getMarkers(
  profile: FirefoxProfile,
  thread: ProfileThread,
  range: [number, number] | null,
  markerType: string | undefined,
  categoryFilter: string | undefined,
  categories: Map<number, string>,
): Marker[] {
  const table = thread.markers ?? {};
  const names = table.name ?? [];
  const startTimes = table.startTime ?? [];
  const endTimes = table.endTime ?? [];
  const phases = table.phase ?? [];
  const categoryIndices = table.category ?? [];
  const dataList = table.data ?? [];
  const strings = stringTable(profile, thread);
  const phaseNames: Record<number, string> = { 0: "instant", 1: "interval", 2: "start", 3: "end" };
  const out: Marker[] = [];
  for (let index = 0; index < names.length; index += 1) {
    const nameIndex = names[index];
    if (nameIndex === null || nameIndex === undefined) continue;
    const name = nameIndex < strings.length ? strings[nameIndex] : `<marker:${nameIndex}>`;
    const start = startTimes[index];
    const end = index < endTimes.length ? endTimes[index] : null;
    if (start === null || start === undefined) continue;
    if (range) {
      if (start > range[1]) continue;
      if (end !== null && end !== undefined && end < range[0]) continue;
      if ((end === null || end === undefined) && start < range[0]) continue;
    }
    const category = categoryName(categories, index < categoryIndices.length ? categoryIndices[index] : null);
    if (categoryFilter && !category.toLowerCase().includes(categoryFilter.toLowerCase())) continue;
    const phaseIndex = index < phases.length ? phases[index] : 0;
    const rawData = index < dataList.length ? dataList[index] : null;
    const data =
      rawData && typeof rawData === "object" && !Array.isArray(rawData)
        ? (rawData as Record<string, unknown>)
        : rawData === null || rawData === undefined
          ? {}
          : { value: rawData };
    if (markerType) {
      const dataType = String((data as Record<string, unknown>).type ?? "");
      const needle = markerType.toLowerCase();
      if (!name.toLowerCase().includes(needle) && !dataType.toLowerCase().includes(needle)) continue;
    }
    const marker: Marker = {
      name,
      category,
      start_ms: Math.round(start * 1000) / 1000,
      data,
      thread_id: thread.tid,
    };
    // Interval markers record `endTime: 0` on their start entry and `startTime: 0` on their end
    // entry, so only a positive span is a real duration.
    if (end !== null && end !== undefined && end >= start) {
      marker.end_ms = Math.round(end * 1000) / 1000;
      marker.duration_ms = Math.round((end - start) * 1000) / 1000;
    }
    if (phaseNames[phaseIndex] !== "instant") marker.phase = phaseNames[phaseIndex];
    out.push(marker);
  }
  return out;
}

function threadSummaries(profile: FirefoxProfile): ThreadSummary[] {
  return profile.threads.map((thread) => {
    const samples = thread.samples ?? {};
    const times = sampleTimes(samples);
    const cpu = (samples.threadCPUDelta ?? []).reduce<number>((sum, delta) => sum + (delta || 0), 0);
    const start = times.length ? times.reduce((min, time) => (time < min ? time : min), times[0]) : 0;
    const end = times.length ? times.reduce((max, time) => (time > max ? time : max), times[0]) : 0;
    return {
      tid: thread.tid,
      name: thread.name ?? "Unknown",
      pid: thread.pid ?? "",
      process_name: thread.processName ?? "",
      is_main_thread: thread.isMainThread ?? false,
      sample_count: samples.length ?? times.length,
      marker_count: thread.markers?.length ?? 0,
      start_time_ms: Math.round(start * 100) / 100,
      end_time_ms: Math.round(end * 100) / 100,
      duration_ms: Math.round((end - start) * 100) / 100,
      cpu_delta_total_ns: cpu,
    };
  });
}

/** A thread by TID (exact) or name substring (case-insensitive); the main thread by default. */
function selectThread(profile: FirefoxProfile, filter: string | number | undefined): ProfileThread {
  const thread = findThread(profile, filter);
  if (!thread) throw new Error(`thread not found: ${filter === undefined ? "main thread" : JSON.stringify(filter)}`);
  return thread;
}

function computeHotspots(
  profile: FirefoxProfile,
  thread: ProfileThread,
  range: [number, number] | null,
  by: "function" | "stack",
  top: number,
): Hotspot[] {
  const samples = getSamples(profile, thread, range, 50);
  if (!samples.length) return [];
  const total = samples.length;
  const selfCounts = new Map<string, number>();
  const selfWeights = new Map<string, number>();
  const totalCounts = new Map<string, number>();
  const totalWeights = new Map<string, number>();
  const bump = (map: Map<string, number>, key: string, by: number): void => {
    map.set(key, (map.get(key) ?? 0) + by);
  };
  for (const sample of samples) {
    if (!sample.stack.length) continue;
    if (by === "stack") {
      const key = sample.stack.slice(0, 10).reverse().join(" -> ");
      bump(selfCounts, key, 1);
      bump(selfWeights, key, sample.weight);
      bump(totalCounts, key, 1);
      bump(totalWeights, key, sample.weight);
      continue;
    }
    const leaf = sample.stack[0];
    bump(selfCounts, leaf, 1);
    bump(selfWeights, leaf, sample.weight);
    const seen = new Set<string>();
    for (const func of sample.stack) {
      if (seen.has(func)) continue;
      bump(totalCounts, func, 1);
      bump(totalWeights, func, sample.weight);
      seen.add(func);
    }
  }
  const names = new Set([...selfCounts.keys(), ...totalCounts.keys()]);
  const entries: Hotspot[] = [];
  for (const name of names) {
    const selfSamples = selfCounts.get(name) ?? 0;
    const totalSamples = totalCounts.get(name) ?? 0;
    entries.push({
      name,
      self_samples: selfSamples,
      self_weight: selfWeights.get(name) ?? 0,
      total_samples: totalSamples,
      total_weight: totalWeights.get(name) ?? 0,
      self_pct: total > 0 ? Math.round((selfSamples / total) * 10000) / 100 : 0,
      total_pct: total > 0 ? Math.round((totalSamples / total) * 10000) / 100 : 0,
    });
  }
  entries.sort((a, b) => b.self_samples - a.self_samples);
  return entries.slice(0, top);
}

function computeTimeline(
  profile: FirefoxProfile,
  thread: ProfileThread,
  range: [number, number] | null,
  bucketMs: number,
): TimelineBucket[] {
  const samples = getSamples(profile, thread, range, 5);
  if (!samples.length) return [];
  let minTime = samples.reduce((min, sample) => (sample.time_ms < min ? sample.time_ms : min), samples[0].time_ms);
  let maxTime = samples.reduce((max, sample) => (sample.time_ms > max ? sample.time_ms : max), samples[0].time_ms);
  if (range) {
    minTime = Math.max(minTime, range[0]);
    maxTime = Math.min(maxTime, range[1]);
  }
  const buckets = new Map<number, Sample[]>();
  for (const sample of samples) {
    const index = Math.floor((sample.time_ms - minTime) / bucketMs);
    const list = buckets.get(index);
    if (list) list.push(sample);
    else buckets.set(index, [sample]);
  }
  return [...buckets.keys()].sort((a, b) => a - b).map((index) => {
    const list = buckets.get(index)!;
    const counts = new Map<string, number>();
    for (const sample of list) {
      if (!sample.stack.length) continue;
      const leaf = sample.stack[0];
      counts.set(leaf, (counts.get(leaf) ?? 0) + 1);
    }
    const top = [...counts.entries()].sort(([, a], [, b]) => b - a).slice(0, 3).map(([name]) => name);
    const start = minTime + index * bucketMs;
    return {
      start_ms: Math.round(start * 100) / 100,
      end_ms: Math.round((start + bucketMs) * 100) / 100,
      sample_count: list.length,
      total_weight: list.reduce((sum, sample) => sum + sample.weight, 0),
      cpu_delta_ns: list.reduce((sum, sample) => sum + (sample.cpu_delta_ns || 0), 0),
      top_functions: top,
    };
  });
}

function checkLimit(limit: number | undefined, fallback: number): number {
  const value = limit === undefined ? fallback : limit;
  if (!Number.isInteger(value) || value < 1) throw new Error("limit must be a positive integer");
  return value;
}

function checkDepth(depth: number | undefined, fallback: number): number {
  const value = depth === undefined ? fallback : depth;
  if (!Number.isInteger(value) || value < 1) throw new Error("max_depth must be a positive integer");
  return value;
}

/**
 * The URL a profile downloads from, without downloading it: `profiler.firefox.com/public/<token>`
 * becomes `https://storage.googleapis.com/profile-store/<token>`, and a
 * `storage.googleapis.com/profile-store/...` URL is returned unchanged. `share.firefox.dev` short
 * links cannot be expanded here (the redirect target is not exposed); pass them to the other
 * functions, which follow the redirect.
 */
export async function resolve_url(args: { url: string }, ctx: Ctx): Promise<{ download_url: string }> {
  const trimmed = args.url.trim();
  if (hostOf(trimmed) === "share.firefox.dev") {
    throw new Error(
      "share.firefox.dev short links cannot be resolved to a download URL here; " +
        "the other functions download them directly (and need a link that redirects to a profile)",
    );
  }
  return { download_url: downloadUrl(trimmed) };
}

/**
 * Download a whole profile (gzip compressed or plain JSON) and save it unchanged to a file: the
 * document keeps `meta`, `threads` and `shared` as the profiler wrote them. `path` is where to
 * save it, relative to the files root or absolute inside it (default
 * `downloads/profslice/<storage id>.json`); the file limit is 100 MiB. The bytes are saved as
 * served, so a gzip-compressed profile stays gzip compressed even under a `.json` name (the
 * analysis functions detect it by content). Returns the saved file's `path`, `bytes` and
 * `mime_type`. Hand that `path` to the analysis functions instead of the URL
 * to avoid downloading it again. Accepted hosts: share.firefox.dev, profiler.firefox.com and
 * storage.googleapis.com.
 */
export async function fetch_profile(
  args: { url: string; path?: string },
  ctx: Ctx,
): Promise<SavedFile> {
  const source = downloadUrl(args.url);
  const saved = await downloadTo(
    ctx,
    source,
    outputPath(args.path, "profslice", profileFilename(source)),
    "profile download",
    { headers: { "user-agent": "profslice/0.1.0" } },
  );
  const result: SavedFile = { path: saved.path, bytes: saved.bytes };
  if (saved.mime_type) result.mime_type = saved.mime_type;
  return result;
}

/**
 * Profile metadata and totals: `product`, `platform`, `interval_ms` (sampling interval), profile
 * `version`, thread and sample/marker counts, and the category names. The cheapest call for a
 * first look. `url` or `path` as in `fetch_profile`.
 */
export async function profile_info(
  args: ProfileSource,
  ctx: Ctx,
): Promise<{
  product: string;
  platform: string | null;
  interval_ms: number;
  version: number;
  thread_count: number;
  total_samples: number;
  total_markers: number;
  categories: (string | undefined)[];
}> {
  const profile = await loadProfile(ctx, args, "profslice.profile_info");
  const meta = profile.meta ?? { interval: 0 };
  const threads = profile.threads ?? [];
  return {
    product: meta.product ?? "Unknown",
    platform: meta.platform ?? null,
    interval_ms: meta.interval ?? 0,
    version: meta.version ?? 0,
    thread_count: threads.length,
    total_samples: threads.reduce((sum, thread) => sum + (thread.samples?.length ?? 0), 0),
    total_markers: threads.reduce((sum, thread) => sum + (thread.markers?.length ?? 0), 0),
    categories: (meta.categories ?? []).map((category) => category.name),
  };
}

/**
 * Every thread with its id, name, process, main-thread flag, sample and marker counts, first/last
 * sample time (ms from profile start), duration and total CPU time (`cpu_delta_total_ns`, ns).
 * Use `tid` or the thread name as `thread` in the other functions. `url` or `path` as in
 * `fetch_profile`.
 */
export async function threads(
  args: ProfileSource,
  ctx: Ctx,
): Promise<{ threads: ThreadSummary[] }> {
  return { threads: threadSummaries(await loadProfile(ctx, args, "profslice.threads")) };
}

/**
 * Samples of one thread with resolved stacks. `thread` is a TID or a name substring; omit it for
 * the main thread. `time_range` limits by sample time in the thread's own clock (`1000-2000`,
 * `1s-2s`, or `1s+500ms` for start plus duration) — check `threads` for `start_time_ms` and
 * `end_time_ms`, which are often in the millions. Each sample has `time_ms`, `stack` (function
 * names, leaf first, up to `max_depth` frames, default 10), `weight` and `cpu_delta_ns` where the
 * platform provides it.
 * `limit` caps samples (default 100) and returns `truncated`. `url` or `path` as in
 * `fetch_profile`.
 */
export async function samples(
  args: ProfileSource & {
    thread?: string | number;
    time_range?: string;
    max_depth?: number;
    limit?: number;
  },
  ctx: Ctx,
): Promise<{ samples: Sample[]; truncated: boolean }> {
  const profile = await loadProfile(ctx, args, "profslice.samples");
  const thread = selectThread(profile, args.thread);
  const all = getSamples(profile, thread, parseTimeRange(args.time_range), checkDepth(args.max_depth, 10));
  const limit = checkLimit(args.limit, 100);
  return { samples: all.slice(0, limit), truncated: all.length > limit };
}

/**
 * Markers (events, intervals, user timings, GC, network, ...) of one thread, with `name`,
 * `category`, `start_ms`, `end_ms`/`duration_ms` for intervals, `phase` and the marker's `data`
 * payload. `marker_type` matches the marker name or its `data.type` (case-insensitive substring);
 * `category` matches the category name; `time_range` is as in `samples`. `limit` caps markers
 * (default 100) and returns `truncated`. `url` or `path` as in `fetch_profile`.
 */
export async function markers(
  args: ProfileSource & {
    thread?: string | number;
    marker_type?: string;
    category?: string;
    time_range?: string;
    limit?: number;
  },
  ctx: Ctx,
): Promise<{ markers: Marker[]; truncated: boolean }> {
  const profile = await loadProfile(ctx, args, "profslice.markers");
  const thread = selectThread(profile, args.thread);
  const all = getMarkers(
    profile,
    thread,
    parseTimeRange(args.time_range),
    args.marker_type,
    args.category,
    categoryMap(profile.meta ?? { interval: 0 }),
  );
  const limit = checkLimit(args.limit, 100);
  return { markers: all.slice(0, limit), truncated: all.length > limit };
}

/**
 * Hot functions (`by: "function"`, default) or hot stacks (`by: "stack"`), most self samples
 * first. Each entry has `self_samples`/`self_weight` (leaf), `total_samples`/`total_weight`
 * (anywhere in the stack) and both percentages of all samples in the thread and range. `top`
 * caps entries (default 20); `thread` and `time_range` are as in `samples`, `url` or `path` as in
 * `fetch_profile`.
 */
export async function hotspots(
  args: ProfileSource & {
    thread?: string | number;
    time_range?: string;
    by?: "function" | "stack";
    top?: number;
  },
  ctx: Ctx,
): Promise<{ hotspots: Hotspot[] }> {
  const profile = await loadProfile(ctx, args, "profslice.hotspots");
  const thread = selectThread(profile, args.thread);
  const top = args.top === undefined ? 20 : args.top;
  if (!Number.isInteger(top) || top < 1) throw new Error("top must be a positive integer");
  return {
    hotspots: computeHotspots(profile, thread, parseTimeRange(args.time_range), args.by ?? "function", top),
  };
}

/**
 * Time-bucketed sample counts for one thread: per `bucket` (`100ms`, `1s`, `2m`; default `1s`)
 * the start/end time, sample count, summed weight, summed CPU time (ns) and up to three most
 * sampled leaf functions. `thread` and `time_range` are as in `samples`, `url` or `path` as in
 * `fetch_profile`.
 */
export async function timeline(
  args: ProfileSource & {
    thread?: string | number;
    time_range?: string;
    bucket?: string;
  },
  ctx: Ctx,
): Promise<{ buckets: TimelineBucket[] }> {
  const bucketMs = parseMillis(args.bucket ?? "1s", "bucket size");
  if (bucketMs <= 0) throw new Error("bucket size must be positive");
  const profile = await loadProfile(ctx, args, "profslice.timeline");
  const thread = selectThread(profile, args.thread);
  return { buckets: computeTimeline(profile, thread, parseTimeRange(args.time_range), bucketMs) };
}
