import type { Ctx } from "../centaur";
import { readJson } from "../_lib/http";

// Reducto document extraction, the HTTP core of tools/research/archiver: upload
// a document, submit an async parse (chunks) and/or an async extract (metadata
// against a JSON schema), then read the job. The tool's other stages are local
// orchestration a cell does itself: DocSend/Drive downloads through a remote
// browser (Playwright/CDP) and Reducto parse+extract of a manifest directory.

const DEFAULT_BASE = "https://platform.reducto.ai";
const JOB_POLL_INTERVAL_SECONDS = 2;
// A tool call is capped at 60 s, so waiting inside one call is bounded.
const DEFAULT_WAIT_SECONDS = 50;
const MAX_WAIT_SECONDS = 55;
const MAX_UPLOAD_BYTES = 8 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES = 8 * 1024 * 1024;

// Public export endpoints, one per Google editor type.
const GOOGLE_EXPORT_FORMATS: Record<string, string> = {
  document: "pdf",
  presentation: "pptx",
  spreadsheets: "xlsx",
};
const GOOGLE_EXPORT_URLS: Record<string, string> = {
  document: "https://docs.google.com/document/d/{file_id}/export?format={fmt}",
  presentation: "https://docs.google.com/presentation/d/{file_id}/export/{fmt}",
  spreadsheets: "https://docs.google.com/spreadsheets/d/{file_id}/export?format={fmt}",
};
const GOOGLE_FILE_URL = "https://drive.google.com/uc?export=download&id={file_id}";

// The python tool's default retrieval settings: variable-size chunks without
// page furniture.
const DEFAULT_CHUNK_MODE = "variable";
const DEFAULT_FILTER_BLOCKS: string[] = ["Header", "Footer", "Page Number"];

// Pitch-deck metadata schema and prompt from the python tool, used when
// `submit_extract` is called without its own schema.
const EXTRACT_SCHEMA: Record<string, unknown> = {
  type: "object",
  properties: {
    company: {
      type: "object",
      properties: {
        name: { type: "string", description: "Company or startup name" },
        aliases: { type: "array", items: { type: "string" } },
        website: { type: "string", description: "Company website URL" },
      },
      required: ["name"],
    },
    deal: {
      type: "object",
      properties: {
        round_label: {
          type: "string",
          enum: ["Pre-Seed", "Seed", "Series A", "Series B", "Series C", "Growth", "Token Round", "M&A", "Unknown"],
        },
        round_freeform: { type: "string" },
        amount_usd: { type: "number" },
        valuation_usd: { type: "number" },
      },
    },
    document: {
      type: "object",
      properties: {
        doc_type: {
          type: "string",
          enum: [
            "PitchDeck", "InvestorMemo", "OnePager", "TermSheet", "FinancialModel",
            "InvestorUpdate", "NDA", "DataroomFile", "Other",
          ],
        },
        title: { type: "string" },
        date: { type: "string" },
      },
    },
    summary: {
      type: "object",
      properties: {
        one_liner: { type: "string" },
        sector_tags: { type: "array", items: { type: "string" } },
        geo: { type: "array", items: { type: "string" } },
      },
    },
  },
  required: ["company", "document"],
};

const EXTRACT_PROMPT =
  "Extract key metadata from this pitch deck or investment document.\n" +
  "Focus on identifying:\n" +
  "- The company name (look for logos, headers, \"About Us\" sections)\n" +
  "- The funding round (Pre-Seed, Seed, Series A, etc.)\n" +
  "- Document type (pitch deck, memo, term sheet, etc.)\n" +
  "- A brief one-liner summary of what the company does\n" +
  "- Relevant sector/industry tags\n\n" +
  "If information is not clearly present, use null or \"Unknown\".";

// The runtime installs TextEncoder in every realm; the es2023 lib does not
// declare it.
declare const TextEncoder: { new (): { encode(input?: string): Uint8Array } };

/** A document stored on Reducto. */
export interface UploadedFile {
  /** Pass to `submit_parse`/`submit_extract` as `input`. */
  file_id: string;
  filename: string;
  size_bytes: number;
}

/** An accepted async job. */
export interface JobHandle {
  job_id: string;
}

/** A parse or extract result chunk, as Reducto sends it. */
export interface ReductoChunk {
  /** Text Reducto recommends for embedding; usually the same as `content`. */
  embed?: string;
  content?: string;
  /** Layout blocks with `bbox.page` (1-based) and coordinates. */
  blocks?: { bbox?: { page?: number; [field: string]: unknown }; [field: string]: unknown }[];
  [field: string]: unknown;
}

/** A Reducto job document. */
export interface ReductoJob {
  job_id?: string;
  /** `Queued`, `Processing`, `Completed` or `Failed`. */
  status: string;
  /** Parse results are `{ result: { type: "url", url } }` or chunks; extract results a list. */
  result?: { result?: unknown; [field: string]: unknown } | unknown;
  [field: string]: unknown;
}

/** One parse chunk, flattened for reading. */
export interface ParsedChunk {
  /** 1-based page of the chunk's first block. */
  page: number;
  chunk_index: number;
  text: string;
}

function base(ctx: Ctx): string {
  return (ctx.env.REDUCTO_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx, extra: Record<string, string> = {}): Record<string, string> {
  const out: Record<string, string> = { accept: "application/json", ...extra };
  // Behind iron-proxy the bearer is injected on the wire; locally it comes from the env.
  if (ctx.env.REDUCTO_API_KEY) out.authorization = `Bearer ${ctx.env.REDUCTO_API_KEY}`;
  return out;
}

function envEnabled(ctx: Ctx, name: string, fallback: boolean): boolean {
  const value = ctx.env[name];
  if (value === undefined) return fallback;
  return !["0", "false", "no", "off"].includes(value.trim().toLowerCase());
}

/** `(file_id, link_type)` for a Google Docs/Drive URL, or null. */
function parseGoogleUrl(url: string): [string, string] | null {
  const editor = /docs\.google\.com\/(document|presentation|spreadsheets)\/d\/([a-zA-Z0-9_-]+)/.exec(url);
  if (editor) return [editor[2], editor[1]];
  const folder = /drive\.google\.com\/drive\/(?:u\/\d+\/)?folders\/([a-zA-Z0-9_-]+)/.exec(url);
  if (folder) return [folder[1], "folder"];
  const file = /drive\.google\.com\/file\/d\/([a-zA-Z0-9_-]+)/.exec(url);
  if (file) return [file[1], "file"];
  const open = /drive\.google\.com\/open\?id=([a-zA-Z0-9_-]+)/.exec(url);
  if (open) return [open[1], "file"];
  return null;
}

/** A filesystem-safe slug, as the python tool writes download names. */
function slugify(text: string): string {
  const slug = text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  return (slug.length > 60 ? slug.slice(0, 60).replace(/-+$/, "") : slug) || "untitled";
}

/** A non-completed job, with Reducto's failure detail when it reports one. */
function unfinished(kind: string, jobId: string, job: ReductoJob): Error {
  const record = job as Record<string, unknown>;
  const detail = record.error ?? record.reason ?? record.failure_reason ?? null;
  let suffix = "";
  if (detail !== null && detail !== undefined) {
    suffix = `: ${typeof detail === "string" ? detail : JSON.stringify(detail).slice(0, 1000)}`;
  }
  return new Error(`Reducto ${kind} job ${jobId} is ${job.status}${suffix}`);
}

async function post(ctx: Ctx, path: string, body: unknown): Promise<Record<string, any>> {
  const res = await ctx.fetch(base(ctx) + path, {
    method: "POST",
    headers: headers(ctx, { "content-type": "application/json" }),
    body: JSON.stringify(body),
  });
  return readJson(res, "Reducto");
}

/**
 * Start a parse job for a stored document: variable-size chunks with page
 * furniture filtered out. `input` is a `file_id` from `upload`; `chunk_mode` and
 * `filter_blocks` override the defaults (`variable`; Header, Footer, Page
 * Number). Poll `job_status`/`wait_for_job`, then read the text with
 * `parse_chunks`.
 */
export async function submit_parse(
  args: { input: string; chunk_mode?: string; filter_blocks?: string[] },
  ctx: Ctx,
): Promise<JobHandle> {
  const body = await post(ctx, "/parse_async", {
    input: args.input,
    retrieval: {
      chunking: { chunk_mode: args.chunk_mode ?? DEFAULT_CHUNK_MODE },
      filter_blocks: args.filter_blocks ?? DEFAULT_FILTER_BLOCKS,
    },
  });
  return { job_id: String(body.job_id) };
}

/**
 * Start an extract job for a stored document. `input` is a `file_id` from
 * `upload`; `schema` is a JSON schema (default: the pitch-deck schema for
 * company {name, aliases, website}, deal {round_label, round_freeform,
 * amount_usd, valuation_usd}, document {doc_type, title, date} and summary
 * {one_liner, sector_tags, geo}); `system_prompt` defaults to the matching
 * instructions; `optimize_for_latency` defaults to true (env
 * EXTRACT_OPTIMIZE_FOR_LATENCY). Read the result with `extract_metadata`.
 */
export async function submit_extract(
  args: { input: string; schema?: Record<string, unknown>; system_prompt?: string; optimize_for_latency?: boolean },
  ctx: Ctx,
): Promise<JobHandle> {
  const body = await post(ctx, "/extract_async", {
    input: args.input,
    instructions: {
      schema: args.schema ?? EXTRACT_SCHEMA,
      system_prompt: args.system_prompt ?? EXTRACT_PROMPT,
    },
    settings: {
      optimize_for_latency: args.optimize_for_latency ?? envEnabled(ctx, "EXTRACT_OPTIMIZE_FOR_LATENCY", true),
    },
  });
  return { job_id: String(body.job_id) };
}

/**
 * Upload one document (PDF, image, Word, PowerPoint or Excel) to Reducto.
 * `content_base64` is the file's bytes; `filename` drives Reducto's type
 * detection, `content_type` defaults to application/octet-stream. Uploads are
 * capped at 8 MiB so the base64 argument fits a cell. Returns the `file_id` for
 * `submit_parse`/`submit_extract`.
 */
export async function upload(
  args: { filename: string; content_base64: string; content_type?: string },
  ctx: Ctx,
): Promise<UploadedFile> {
  const bytes = ctx.base64.decode(args.content_base64);
  if (bytes.length > MAX_UPLOAD_BYTES) {
    throw new Error(`Upload exceeds the ${MAX_UPLOAD_BYTES / (1024 * 1024)} MiB limit`);
  }
  const filename = args.filename.replace(/[\r\n"\\]/g, "_");
  const contentType = args.content_type ?? "application/octet-stream";
  // Math.random is enough: the boundary only has to miss the payload.
  const boundary = `----CentaurCodeMode${Date.now()}${Math.floor(Math.random() * 1e9)}`;
  const body = concatBytes([
    `--${boundary}\r\n`,
    `Content-Disposition: form-data; name="file"; filename="${filename}"\r\n`,
    `Content-Type: ${contentType}\r\n\r\n`,
    bytes,
    `\r\n--${boundary}--\r\n`,
  ]);
  const res = await ctx.fetch(`${base(ctx)}/upload`, {
    method: "POST",
    headers: headers(ctx, { "content-type": `multipart/form-data; boundary=${boundary}` }),
    body,
  });
  const response = await readJson(res, "Reducto");
  return { file_id: String(response.file_id), filename: args.filename, size_bytes: bytes.length };
}

/**
 * One job's current state: `status` (`Queued`, `Processing`, `Completed`,
 * `Failed`) plus Reducto's `result` and progress fields as sent. Completion is
 * usually seconds for a parse and tens of seconds for an extract.
 */
export async function job_status(
  args: { job_id: string },
  ctx: Ctx,
): Promise<{ job: ReductoJob }> {
  const res = await ctx.fetch(`${base(ctx)}/job/${encodeURIComponent(args.job_id)}`, {
    headers: headers(ctx),
  });
  return { job: (await readJson(res, "Reducto")) as ReductoJob };
}

/**
 * Poll a job until it is `Completed` or `Failed`, or `timeout_seconds` (default
 * 50, at most 55 because one call is capped at 60 s) expires. Returns the job as
 * `job_status` does; on timeout call `wait_for_job` again rather than resubmitting.
 */
export async function wait_for_job(
  args: { job_id: string; timeout_seconds?: number },
  ctx: Ctx,
): Promise<{ job: ReductoJob }> {
  const timeout = Math.min(args.timeout_seconds ?? DEFAULT_WAIT_SECONDS, MAX_WAIT_SECONDS);
  const started = Date.now();
  for (;;) {
    const { job } = await job_status({ job_id: args.job_id }, ctx);
    if (job.status === "Completed" || job.status === "Failed") return { job };
    if (Date.now() - started + JOB_POLL_INTERVAL_SECONDS * 1000 > timeout * 1000) return { job };
    await ctx.sleep(JOB_POLL_INTERVAL_SECONDS * 1000);
  }
}

/**
 * A completed parse job's text: `chunks` (`page`, `chunk_index`, `text`;
 * whitespace-only chunks dropped), the same text joined with blank lines as
 * `full_text`, and `content_hash` (SHA-256 of `full_text`, null when empty), the
 * manifest identity field. Reducto may store chunks behind a result URL, which
 * is fetched here. A failed or unfinished job throws.
 */
export async function parse_chunks(
  args: { job_id: string },
  ctx: Ctx,
): Promise<{ chunks: ParsedChunk[]; full_text: string; content_hash: string | null }> {
  const { job } = await job_status({ job_id: args.job_id }, ctx);
  if (job.status !== "Completed") throw unfinished("parse", args.job_id, job);
  const outer = (job.result ?? {}) as Record<string, any>;
  const inner = (outer.result ?? {}) as Record<string, any>;
  let raw: ReductoChunk[] = [];
  if (inner.type === "url" && typeof inner.url === "string") {
    const res = await ctx.fetch(inner.url, { headers: headers(ctx) });
    raw = (await readJson(res, "Reducto chunks")) as ReductoChunk[];
  } else if (Array.isArray(inner.chunks)) {
    raw = inner.chunks as ReductoChunk[];
  }
  const chunks: ParsedChunk[] = [];
  for (let index = 0; index < raw.length; index += 1) {
    const chunk = raw[index];
    const text = chunk.embed || chunk.content || "";
    if (!text.trim()) continue;
    chunks.push({ page: chunk.blocks?.[0]?.bbox?.page ?? 1, chunk_index: index, text });
  }
  const fullText = chunks.map((chunk) => chunk.text).join("\n\n");
  return { chunks, full_text: fullText, content_hash: fullText ? sha256Hex(fullText) : null };
}

/**
 * A completed extract job's metadata object: the first extracted item as Reducto
 * returns it (the `company`, `deal`, `document` and `summary` keys for the
 * default pitch-deck schema). A failed or unfinished job throws.
 */
export async function extract_metadata(
  args: { job_id: string },
  ctx: Ctx,
): Promise<{ metadata: Record<string, any> }> {
  const { job } = await job_status({ job_id: args.job_id }, ctx);
  if (job.status !== "Completed") throw unfinished("extract", args.job_id, job);
  const outer = (job.result ?? {}) as Record<string, any>;
  const inner = outer.result;
  if (Array.isArray(inner)) {
    const first = inner[0];
    return { metadata: first && typeof first === "object" ? (first as Record<string, any>) : {} };
  }
  return { metadata: inner && typeof inner === "object" ? (inner as Record<string, any>) : {} };
}

/**
 * Export a publicly shared Google Doc, Slides, Sheets or Drive file with
 * Google's plain-HTTP export endpoints (`docs.google.com/.../export`,
 * `drive.google.com/uc?export=download`). `url` is any Google Docs or Drive
 * URL; `title` names the file (default: the link type) and `export_format`
 * overrides the default (document→pdf, presentation→pptx, spreadsheets→xlsx).
 * Returns the bytes as base64 with a suggested `filename` and `size_bytes` —
 * pass `content_base64` to `upload` to parse it. Public sharing is required:
 * Google answers with HTML for private documents, Drive folders cannot be listed
 * without OAuth, and files over 8 MiB are refused. Google-native files only
 * export this way; authenticated `gog` downloads are not available here.
 */
export async function google_export(
  args: { url: string; title?: string; export_format?: string },
  ctx: Ctx,
): Promise<{
  file_id: string;
  link_type: string;
  filename: string;
  content_type: string;
  size_bytes: number;
  content_base64: string;
}> {
  const parsed = parseGoogleUrl(args.url);
  if (!parsed) throw new Error("Not a recognised Google Docs or Drive URL");
  const [fileId, linkType] = parsed;
  if (linkType === "folder") {
    throw new Error("Google Drive folder listing requires Drive OAuth, which this connector does not have");
  }
  let format = "";
  let url = "";
  if (linkType === "file") {
    url = GOOGLE_FILE_URL.replace("{file_id}", fileId);
  } else {
    format = args.export_format ?? GOOGLE_EXPORT_FORMATS[linkType];
    url = GOOGLE_EXPORT_URLS[linkType].replace("{file_id}", fileId).replace("{fmt}", format);
  }
  const res = await ctx.fetch(url, { headers: { accept: "*/*" } });
  if (res.status === 401 || res.status === 403) throw new Error(`Access denied (HTTP ${res.status})`);
  if (res.status === 404) throw new Error("Document not found (HTTP 404)");
  if (!res.ok) throw new Error(`Google export failed with status ${res.status}`);
  const contentType = res.headers["content-type"] ?? "";
  if (contentType.includes("text/html")) {
    throw new Error(
      "Google returned HTML instead of the file: the document is not publicly shared, or Drive requires the large-file confirmation",
    );
  }
  const bytes = await res.bytes();
  if (bytes.length > MAX_DOWNLOAD_BYTES) {
    throw new Error(`Exported file exceeds the ${MAX_DOWNLOAD_BYTES / (1024 * 1024)} MiB limit`);
  }
  const named = /filename\*?=(?:UTF-8'')?"?([^";]+)"?/i.exec(res.headers["content-disposition"] ?? "");
  let filename = "";
  if (named) {
    filename = named[1].trim().replace(/[<>:"/\\|?*]/g, "_");
  }
  const title = args.title ?? linkType;
  if (!filename) filename = `${slugify(title)}__${fileId.slice(0, 10)}.${format || fileExtension(title)}`;
  return {
    file_id: fileId,
    link_type: linkType,
    filename,
    content_type: contentType || "application/octet-stream",
    size_bytes: bytes.length,
    content_base64: ctx.base64.encode(bytes),
  };
}

/** The extension of a Drive filename, without the dot; `bin` when there is none. */
function fileExtension(name: string): string {
  const match = /\.([A-Za-z0-9]{1,8})$/.exec(name);
  return match ? match[1].toLowerCase() : "bin";
}

/** Join strings and byte arrays into one body. */
function concatBytes(parts: (string | Uint8Array)[]): Uint8Array {
  const encoder = new TextEncoder();
  const pieces = parts.map((part) => (typeof part === "string" ? encoder.encode(part) : part));
  let total = 0;
  for (const piece of pieces) total += piece.length;
  const out = new Uint8Array(total);
  let offset = 0;
  for (const piece of pieces) {
    out.set(piece, offset);
    offset += piece.length;
  }
  return out;
}

// SHA-256, to record the content hash the python manifest stores. The cell
// runtime has no crypto API, and a hash has to match across ports.
const SHA256_K: number[] = [
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/** Hex SHA-256 of a UTF-8 string. */
function sha256Hex(input: string): string {
  const bytes = new TextEncoder().encode(input);
  const bitLength = bytes.length * 8;
  const padded = new Uint8Array((((bytes.length + 9) >> 6) + 1) << 6);
  padded.set(bytes);
  padded[bytes.length] = 0x80;
  const high = Math.floor(bitLength / 0x100000000);
  for (let index = 0; index < 4; index += 1) {
    padded[padded.length - 8 + index] = (high >>> (24 - index * 8)) & 0xff;
    padded[padded.length - 4 + index] = (bitLength >>> (24 - index * 8)) & 0xff;
  }
  const state = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const words = new Array<number>(64).fill(0);
  for (let offset = 0; offset < padded.length; offset += 64) {
    for (let index = 0; index < 16; index += 1) {
      const at = offset + index * 4;
      words[index] = ((padded[at] << 24) | (padded[at + 1] << 16) | (padded[at + 2] << 8) | padded[at + 3]) >>> 0;
    }
    for (let index = 16; index < 64; index += 1) {
      const a = words[index - 15];
      const b = words[index - 2];
      const s0 = ((a >>> 7) | (a << 25)) ^ ((a >>> 18) | (a << 14)) ^ (a >>> 3);
      const s1 = ((b >>> 17) | (b << 15)) ^ ((b >>> 19) | (b << 13)) ^ (b >>> 10);
      words[index] = (words[index - 16] + s0 + words[index - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, h] = state;
    for (let index = 0; index < 64; index += 1) {
      const S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const ch = (e & f) ^ (~e & g);
      const temp1 = (h + S1 + ch + SHA256_K[index] + words[index]) >>> 0;
      const S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const temp2 = (S0 + maj) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + temp1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temp1 + temp2) >>> 0;
    }
    state[0] = (state[0] + a) >>> 0;
    state[1] = (state[1] + b) >>> 0;
    state[2] = (state[2] + c) >>> 0;
    state[3] = (state[3] + d) >>> 0;
    state[4] = (state[4] + e) >>> 0;
    state[5] = (state[5] + f) >>> 0;
    state[6] = (state[6] + g) >>> 0;
    state[7] = (state[7] + h) >>> 0;
  }
  return state.map((word) => (word >>> 0).toString(16).padStart(8, "0")).join("");
}
