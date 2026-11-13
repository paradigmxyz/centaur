import type { Ctx } from "../centaur";
import { extensionFor, outputPath, requireFiles, saveBytes } from "../_lib/files";

// Gemini image generation through the Generative Language REST API
// (`generateContent`). Generated images come back inline from Gemini as base64,
// so they are decoded in the realm and written to files under the files root:
// the caller gets paths and passes paths back in to edit them.

const DEFAULT_BASE = "https://generativelanguage.googleapis.com/v1beta";

/** Short model names accepted by `model`, mapped to Gemini model IDs. */
const MODELS: Record<string, string> = {
  flash: "gemini-2.5-flash-image",
  pro: "gemini-3-pro-image-preview",
};

const DEFAULT_MODEL = "flash";

/** One image model, by short name and full Gemini model ID. */
export interface ModelInfo {
  name: string;
  id: string;
}

/** A file this connector saved. */
export interface SavedFile {
  /** Absolute path in the sandbox. */
  path: string;
  bytes: number;
  mime_type?: string;
}

/** A generated image, saved to a file. */
export interface GeneratedImage {
  /** Absolute path of the saved image in the sandbox. */
  path: string;
  bytes: number;
  /** Such as `image/png` or `image/jpeg`. */
  mime_type: string;
  /** True for intermediate "thinking" images Gemini emits before the final one. */
  thought?: boolean;
}

/** One `generateContent` call: the images it saved plus any text it wrote. */
export interface ImageGenerationResult {
  /** The model that answered (Gemini's `modelVersion`). */
  model: string;
  /** Saved images in candidate order; empty when the model answered with text only. */
  images: GeneratedImage[];
  /** Text parts of the candidate, in order (often the model's caption or refusal). */
  text: string[];
  /** Why the candidate stopped, such as `STOP`, `SAFETY` or `IMAGE_SAFETY`. */
  finish_reason?: string;
  /** Set when Gemini refused the prompt, for example `blockReason: "PROHIBITED_CONTENT"`. */
  prompt_feedback?: PromptFeedback;
  /** Token counts Gemini reported. */
  usage_metadata?: UsageMetadata;
}

/** Gemini's prompt-level refusal block. */
export interface PromptFeedback {
  blockReason?: string;
  blockReasonMessage?: string;
  [field: string]: unknown;
}

/** Gemini's token accounting for one call. */
export interface UsageMetadata {
  promptTokenCount?: number;
  candidatesTokenCount?: number;
  totalTokenCount?: number;
  [field: string]: unknown;
}

interface Part {
  text?: string;
  thought?: boolean;
  inlineData?: { mimeType?: string; data?: string };
  [field: string]: unknown;
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

/** The last segment of a path. */
function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || "file";
}

/** Split `name.ext`; an extension needs a dot after the first character. */
function splitExtension(name: string): { stem: string; ext: string } {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? { stem: name.slice(0, dot), ext: name.slice(dot) } : { stem: name, ext: "" };
}

/** `image/png` and friends, from a file extension. */
function mimeForPath(path: string): string | undefined {
  const ext = splitExtension(basename(path)).ext.toLowerCase();
  const known: Record<string, string> = {
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".webp": "image/webp",
    ".gif": "image/gif",
  };
  return known[ext];
}

/**
 * Where the image at `index` goes: the requested `path` for the first one (later ones get a
 * `-2`, `-3` suffix), otherwise `downloads/nano_banana/<stem>-<n>.<ext>`.
 */
function imagePath(requested: string | undefined, index: number, mime_type: string, stem: string): string {
  if (!requested) {
    return outputPath(undefined, "nano_banana", `${stem}-${index + 1}.${extensionFor(mime_type)}`);
  }
  if (index === 0) return requested;
  const slash = requested.lastIndexOf("/");
  const parts = splitExtension(basename(requested));
  return `${requested.slice(0, slash + 1)}${parts.stem}-${index + 1}${parts.ext}`;
}

/** Gemini answers errors as `{ error: { code, message, status } }`; surface its message. */
async function generateContent(
  ctx: Ctx,
  model: string,
  body: unknown,
): Promise<Record<string, any>> {
  const base = (ctx.env.GEMINI_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
  const res = await ctx.fetch(`${base}/models/${encodeURIComponent(model)}:generateContent`, {
    method: "POST",
    headers: headers(ctx),
    body: JSON.stringify(body),
  });
  const text = await res.text();
  let payload: Record<string, any>;
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    throw new Error(`Gemini ${model} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  }
  if (!res.ok) {
    const error = payload.error;
    const message = typeof error === "string" ? error : error?.message || text;
    throw new Error(`Gemini ${model} returned HTTP ${res.status}: ${message}`);
  }
  return payload;
}

async function generateImage(
  ctx: Ctx,
  model: string,
  parts: Part[],
  requested_path: string | undefined,
  stem: string,
  aspect_ratio: string | undefined,
  image_size: string | undefined,
): Promise<ImageGenerationResult> {
  const imageConfig: Record<string, string> = {};
  if (aspect_ratio) imageConfig.aspectRatio = aspect_ratio;
  if (image_size) imageConfig.imageSize = image_size;
  const generationConfig: Record<string, unknown> = { responseModalities: ["IMAGE", "TEXT"] };
  if (Object.keys(imageConfig).length) generationConfig.imageConfig = imageConfig;

  const payload = await generateContent(ctx, model, {
    contents: [{ role: "user", parts }],
    generationConfig,
  });

  const candidate = payload.candidates?.[0] ?? {};
  const result: ImageGenerationResult = {
    model: payload.modelVersion || model,
    images: [],
    text: [],
  };
  for (const part of (candidate.content?.parts ?? []) as Part[]) {
    if (part.text) result.text.push(part.text);
    if (part.inlineData?.data) {
      const mime_type = part.inlineData.mimeType || "image/png";
      // Gemini inlines the image as base64 in its JSON; the file itself goes to disk.
      const saved: SavedFile = await saveBytes(
        ctx,
        imagePath(requested_path, result.images.length, mime_type, stem),
        ctx.base64.decode(part.inlineData.data),
        "nano_banana (saving the generated image)",
        mime_type,
      );
      const image: GeneratedImage = { path: saved.path, bytes: saved.bytes, mime_type };
      if (part.thought) image.thought = true;
      result.images.push(image);
    }
  }
  if (candidate.finishReason) result.finish_reason = candidate.finishReason;
  if (payload.promptFeedback) result.prompt_feedback = payload.promptFeedback;
  if (payload.usageMetadata) result.usage_metadata = payload.usageMetadata;
  return result;
}

/**
 * Generate an image from a text prompt and save it as a file. Returns the `model` that answered,
 * the saved `images` (`path`, `bytes`, `mime_type`; `thought` marks intermediate "thinking"
 * images) and any `text` Gemini wrote; `prompt_feedback.blockReason` or `finish_reason` explains a
 * refusal, and `images` is empty when Gemini answered with text only. `path` is where to save the
 * first image, relative to the files root or absolute inside it (default
 * `downloads/nano_banana/image-1.<ext>`); further images get `-2`, `-3` names beside it.
 *
 * `model`: `flash` (fast, default, gemini-2.5-flash-image) or `pro`
 * (gemini-3-pro-image-preview, higher quality, up to 4K); a full Gemini model ID also works.
 * `aspect_ratio`: 1:1, 2:3, 3:2, 3:4, 4:3, 9:16, 16:9 or 21:9.
 * `image_size`: 1K (default), 2K or 4K; larger than 1K is pro-model territory.
 * The image arrives inline in Gemini's JSON, so a single response must stay inside the runtime's
 * 10 MiB response limit; the saved file may be up to 100 MiB.
 */
export async function generate(
  args: {
    prompt: string;
    path?: string;
    model?: string;
    aspect_ratio?: string;
    image_size?: string;
  },
  ctx: Ctx,
): Promise<ImageGenerationResult> {
  if (!args.prompt || !args.prompt.trim()) throw new Error("prompt is required");
  requireFiles(ctx, "nano_banana.generate");
  return generateImage(
    ctx,
    modelId(args.model),
    [{ text: args.prompt }],
    args.path,
    "image",
    args.aspect_ratio,
    args.image_size,
  );
}

/**
 * Edit (or re-style) an image with a text prompt. `image` is the input file's path in the
 * sandbox (read and inlined into the Gemini request); `mime_type` defaults from the path's
 * extension, then `image/png` (use `image/jpeg`, `image/gif` or `image/webp` types for those).
 * The edited image is saved to `path` (default `downloads/nano_banana/edited-<input name>.<ext>`),
 * and the result has the same shape as `generate`'s; `model`, `aspect_ratio` and `image_size`
 * mean the same as there.
 */
export async function edit(
  args: {
    image: string;
    prompt: string;
    path?: string;
    mime_type?: string;
    model?: string;
    aspect_ratio?: string;
    image_size?: string;
  },
  ctx: Ctx,
): Promise<ImageGenerationResult> {
  if (!args.image || !args.image.trim()) throw new Error("image is required (path of the image to edit)");
  if (!args.prompt || !args.prompt.trim()) throw new Error("prompt is required");
  const files = requireFiles(ctx, "nano_banana.edit");
  if (!(await files.stat(args.image))) throw new Error(`no such file: ${args.image}`);
  const mime_type = args.mime_type || mimeForPath(args.image) || "image/png";
  const imagePart: Part = {
    // Gemini takes the input image inline as base64, so the file is read and encoded here.
    inlineData: { mimeType: mime_type, data: ctx.base64.encode(await files.read(args.image)) },
  };
  return generateImage(
    ctx,
    modelId(args.model),
    [imagePart, { text: args.prompt }],
    args.path,
    `edited-${splitExtension(basename(args.image)).stem}`,
    args.aspect_ratio,
    args.image_size,
  );
}

/** The image models this connector offers: short `name` (for `model`) and full Gemini model `id`. */
export async function list_models(args: {}, ctx: Ctx): Promise<{ models: ModelInfo[] }> {
  const models: ModelInfo[] = [];
  for (const name of Object.keys(MODELS)) {
    models.push({ name, id: MODELS[name] });
  }
  return { models };
}
