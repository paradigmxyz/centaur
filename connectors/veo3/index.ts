import type { Ctx } from "../centaur";

// Veo 3 video generation through the Generative Language REST API
// (`models/{model}:predictLongRunning` + operation polling). Video generation
// takes minutes, so this connector exposes start / status / download instead of
// blocking: start returns an operation name, poll it with `operation_status` (or
// `wait_for_operation` for a bounded wait), then fetch the video with
// `download_video`.

const DEFAULT_BASE = "https://generativelanguage.googleapis.com/v1beta";

/** Short model names accepted by `model`, mapped to Gemini model IDs. */
const MODELS: Record<string, string> = {
  full: "veo-3.1-generate-preview",
  fast: "veo-3.1-fast-generate-preview",
};

const DEFAULT_MODEL = "full";

/** One Veo model, by short name and full Gemini model ID. */
export interface ModelInfo {
  name: string;
  id: string;
  description: string;
}

/** A video Gemini produced or accepted as input. */
export interface VideoSource {
  /** Download URI, for example `https://…/v1beta/files/abc:download?alt=media`. */
  uri?: string;
  /** Inline base64 video bytes, when Gemini returns them instead of a URI. */
  encodedVideo?: string;
  /** Encoding, such as `video/mp4`. */
  encoding?: string;
  [field: string]: unknown;
}

/** One generated sample in a finished operation. */
export interface GeneratedSample {
  video?: VideoSource;
  [field: string]: unknown;
}

/** The generate-video payload inside a finished operation. */
export interface GenerateVideoResponse {
  generatedSamples?: GeneratedSample[];
  /** How many samples the responsible-AI filter dropped. */
  raiMediaFilteredCount?: number;
  /** Why the filter dropped them. */
  raiMediaFilteredReasons?: string[];
  [field: string]: unknown;
}

/** A long-running-operation failure. */
export interface OperationError {
  code?: number;
  message?: string;
  status?: string;
  [field: string]: unknown;
}

/** Gemini's long-running operation object, as the API sends it. */
export interface VeoOperation {
  /** Resource name to poll, such as `models/veo-3.1-generate-preview/operations/abc`. */
  name: string;
  done?: boolean;
  metadata?: { [field: string]: unknown };
  error?: OperationError;
  response?: {
    generateVideoResponse?: GenerateVideoResponse;
    generatedVideos?: GeneratedSample[];
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** An operation plus the video URI and failure message pulled out of it for convenience. */
export interface OperationStatus {
  /** The operation exactly as Gemini returned it. */
  operation: VeoOperation;
  done: boolean;
  /** Download URI of the first generated video; absent until the operation finishes. */
  video_uri?: string;
  /** Encoding of that video, such as `video/mp4`. */
  mime_type?: string;
  /** Gemini's failure message when the operation failed. */
  error?: string;
}

/** A downloaded video, inline as base64 when it fits the caller's byte budget. */
export interface DownloadedVideo {
  uri: string;
  mime_type?: string;
  /** Raw video size in bytes. */
  bytes: number;
  /** Base64 (standard alphabet) of the video; present when `bytes` fits `max_bytes`. */
  data_base64?: string;
}

function base(ctx: Ctx): string {
  return (ctx.env.GEMINI_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx): Record<string, string> {
  const value: Record<string, string> = { "content-type": "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.GOOGLE_API_KEY) value["x-goog-api-key"] = ctx.env.GOOGLE_API_KEY;
  return value;
}

function modelId(model: string | undefined): string {
  const key = (model || DEFAULT_MODEL).trim();
  return MODELS[key] !== undefined ? MODELS[key] : key;
}

/** `data:` URIs and line-wrapped base64 are accepted; the string is passed through otherwise. */
function base64Body(value: string): string {
  const trimmed = value.trim();
  const comma = trimmed.startsWith("data:") ? trimmed.indexOf(",") : -1;
  return (comma >= 0 ? trimmed.slice(comma + 1) : trimmed).replace(/[\s\r\n]+/g, "");
}

/** Veo takes image and video inputs as `{ bytesBase64Encoded, mimeType }` inline blobs. */
function blob(data: string, mime_type: string | undefined, fallback: string): Record<string, string> {
  return { bytesBase64Encoded: base64Body(data), mimeType: mime_type || fallback };
}

/** Gemini answers errors as `{ error: { code, message, status } }`; surface its message. */
async function request(
  ctx: Ctx,
  path: string,
  init: { method?: string; body?: unknown },
): Promise<Record<string, any>> {
  const res = await ctx.fetch(`${base(ctx)}${path}`, {
    method: init.method || "GET",
    headers: headers(ctx),
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
  });
  const text = await res.text();
  let payload: Record<string, any>;
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    throw new Error(`Gemini returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  }
  if (!res.ok) {
    const error = payload.error;
    const message = typeof error === "string" ? error : error?.message || text;
    throw new Error(`Gemini returned HTTP ${res.status}: ${message}`);
  }
  return payload;
}

async function start(ctx: Ctx, model: string, body: unknown): Promise<{ operation: VeoOperation }> {
  const payload = await request(ctx, `/models/${encodeURIComponent(model)}:predictLongRunning`, {
    method: "POST",
    body,
  });
  return { operation: payload as VeoOperation };
}

/** Pull the first video URI (and its encoding) out of a finished operation. */
function status(operation: VeoOperation): OperationStatus {
  const result: OperationStatus = { operation, done: operation.done === true };
  if (operation.error?.message) result.error = operation.error.message;
  const response = operation.response ?? {};
  const samples = response.generateVideoResponse?.generatedSamples ?? response.generatedVideos ?? [];
  const first = samples[0]?.video;
  if (first?.uri) result.video_uri = first.uri;
  if (first?.encoding) result.mime_type = first.encoding;
  return result;
}

/**
 * Start a text-to-video (or image-to-video) generation. Returns the long-running operation to
 * poll with `operation_status` / `wait_for_operation`; it is usually not done for a minute or
 * more. `model`: `full` (default, veo-3.1-generate-preview, highest quality) or `fast`
 * (veo-3.1-fast-generate-preview); a full Gemini model ID also works.
 * `aspect_ratio`: `16:9` (default), `9:16` or `1:1`. `resolution`: `720p` (default), `1080p` or
 * `4k`. `image` is an optional first frame as base64 (standard alphabet; `data:` URI prefix
 * accepted) with `mime_type` (default `image/png`); `reference_images` are optional style or
 * asset references (`reference_type`: `asset` default, or `style`).
 */
export async function start_video_generation(
  args: {
    prompt: string;
    model?: string;
    aspect_ratio?: string;
    resolution?: string;
    image?: { data: string; mime_type?: string };
    reference_images?: { data: string; mime_type?: string; reference_type?: string }[];
  },
  ctx: Ctx,
): Promise<{ operation: VeoOperation }> {
  if (!args.prompt || !args.prompt.trim()) throw new Error("prompt is required");
  const instance: Record<string, unknown> = { prompt: args.prompt };
  if (args.image) instance.image = blob(args.image.data, args.image.mime_type, "image/png");
  const parameters: Record<string, unknown> = {};
  if (args.aspect_ratio) parameters.aspectRatio = args.aspect_ratio;
  if (args.resolution) parameters.resolution = args.resolution;
  const references = args.reference_images ?? [];
  if (references.length) {
    parameters.referenceImages = references.map((reference) => ({
      image: blob(reference.data, reference.mime_type, "image/png"),
      referenceType: reference.reference_type || "asset",
    }));
  }
  return start(ctx, modelId(args.model), { instances: [instance], parameters });
}

/**
 * Start extending an existing video with what happens next. `video` is the clip's bytes in
 * base64 (standard alphabet; `data:` prefix accepted) with `mime_type` (default `video/mp4`);
 * it goes inline in the request, so keep it small — the Gemini API may refuse large inputs or
 * refuse video input for some models, and its error is surfaced as thrown. `prompt` describes
 * the continuation. Returns the long-running operation to poll with `operation_status`.
 */
export async function start_video_extension(
  args: {
    video: string;
    prompt: string;
    mime_type?: string;
    model?: string;
  },
  ctx: Ctx,
): Promise<{ operation: VeoOperation }> {
  if (!args.video || !args.video.trim()) throw new Error("video is required (base64 video bytes)");
  if (!args.prompt || !args.prompt.trim()) throw new Error("prompt is required");
  return start(ctx, modelId(args.model), {
    instances: [{ prompt: args.prompt, video: blob(args.video, args.mime_type, "video/mp4") }],
  });
}

/**
 * Poll one operation by the `name` that `start_video_generation` returned (such as
 * `models/veo-3.1-generate-preview/operations/abc`). Returns the operation untouched plus
 * `done`, and once it finishes the first `video_uri` (`operation_status`), its `mime_type`, or
 * the failure `error`. Poll every 15–30 s; a finished operation keeps its result, so re-reading
 * is free.
 */
export async function operation_status(
  args: { operation: string },
  ctx: Ctx,
): Promise<OperationStatus> {
  const name = (args.operation || "").trim().replace(/^\/+/, "");
  if (!name) throw new Error("operation is required (the `name` from start_video_generation)");
  return status((await request(ctx, `/${name}`, {})) as VeoOperation);
}

/**
 * Poll `operation_status` until the operation finishes or `timeout_ms` runs out (default 45000,
 * at most 55000 because one call may not exceed 60 s) and return the last status. `done: false`
 * means it is still rendering: keep polling with `operation_status` in a later cell.
 * `poll_interval_ms` defaults to 10000.
 */
export async function wait_for_operation(
  args: { operation: string; timeout_ms?: number; poll_interval_ms?: number },
  ctx: Ctx,
): Promise<OperationStatus> {
  const timeout = Math.min(Math.max(args.timeout_ms ?? 45000, 0), 55000);
  const interval = Math.max(args.poll_interval_ms ?? 10000, 1000);
  const started = Date.now();
  let current = await operation_status(args, ctx);
  while (!current.done && !current.error && Date.now() - started + interval <= timeout) {
    await ctx.sleep(interval);
    current = await operation_status(args, ctx);
  }
  return current;
}

/** `…/files/abc` needs `:download?alt=media`; a URI that already has it is used as is. */
function downloadUri(uri: string): string {
  const trimmed = uri.trim();
  if (trimmed.includes("alt=media") || trimmed.includes(":download")) return trimmed;
  return `${trimmed}:download?alt=media`;
}

/**
 * Download a generated video from the `video_uri` of a finished operation. The bytes come back
 * as base64 (`data_base64`) when the video is at most `max_bytes` (default 7000000, the largest
 * that stays inside the runtime's 10 MiB result cap); otherwise only the size and URI are
 * returned, which the caller can hand to another service. A single HTTP response is capped at
 * 10 MiB, so larger videos cannot be fetched here.
 */
export async function download_video(
  args: { uri: string; max_bytes?: number },
  ctx: Ctx,
): Promise<DownloadedVideo> {
  if (!args.uri || !args.uri.trim()) throw new Error("uri is required (the operation's video_uri)");
  const uri = downloadUri(args.uri);
  const res = await ctx.fetch(uri, { headers: headers(ctx) });
  if (!res.ok) throw new Error(`Video download returned HTTP ${res.status}: ${(await res.text()).slice(0, 1000)}`);
  const bytes = await res.bytes();
  const result: DownloadedVideo = { uri, bytes: bytes.length };
  if (res.headers["content-type"]) result.mime_type = res.headers["content-type"].split(";")[0].trim();
  if (bytes.length <= (args.max_bytes ?? 7000000)) result.data_base64 = ctx.base64.encode(bytes);
  return result;
}

/** The video models this connector offers: short `name` (for `model`), full Gemini model `id`. */
export async function list_models(args: {}, ctx: Ctx): Promise<{ models: ModelInfo[] }> {
  const models: ModelInfo[] = [
    {
      name: "full",
      id: MODELS.full,
      description: "Highest quality video generation (slower, more expensive)",
    },
    {
      name: "fast",
      id: MODELS.fast,
      description: "Fast video generation (lower quality)",
    },
  ];
  return { models };
}
