import type { Ctx } from "../centaur";

// Gemini image generation through the Generative Language REST API
// (`generateContent`). Generated images come back inline from Gemini as base64,
// so they are returned to the cell as `{ mime_type, data }` instead of being
// written to a file: the caller decides what to do with the bytes.

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

/** A generated image. */
export interface GeneratedImage {
  /** Such as `image/png` or `image/jpeg`. */
  mime_type: string;
  /** Base64 in the standard alphabet, exactly as Gemini returned it. */
  data: string;
  /** True for intermediate "thinking" images Gemini emits before the final one. */
  thought?: boolean;
}

/** One `generateContent` call: the images it produced plus any text it wrote. */
export interface ImageGenerationResult {
  /** The model that answered (Gemini's `modelVersion`). */
  model: string;
  /** Base64 images in candidate order; empty when the model answered with text only. */
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

/** `data:` URIs and line-wrapped base64 are accepted; both alphabets pass through. */
function base64Body(value: string): string {
  const trimmed = value.trim();
  const comma = trimmed.startsWith("data:") ? trimmed.indexOf(",") : -1;
  return (comma >= 0 ? trimmed.slice(comma + 1) : trimmed).replace(/[\s\r\n]+/g, "");
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
      const image: GeneratedImage = {
        mime_type: part.inlineData.mimeType || "image/png",
        data: part.inlineData.data,
      };
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
 * Generate an image from a text prompt. Returns the images inline as base64 (`images[].data`
 * with `images[].mime_type`) plus any text Gemini wrote; `prompt_feedback.blockReason` or
 * `finish_reason` explains a refusal, and `images` is empty when Gemini answered with text only.
 *
 * `model`: `flash` (fast, default, gemini-2.5-flash-image) or `pro`
 * (gemini-3-pro-image-preview, higher quality, up to 4K); a full Gemini model ID also works.
 * `aspect_ratio`: 1:1, 2:3, 3:2, 3:4, 4:3, 9:16, 16:9 or 21:9.
 * `image_size`: 1K (default), 2K or 4K; larger than 1K is pro-model territory.
 * The whole response must fit the runtime's 10 MiB result cap, base64 included.
 */
export async function generate(
  args: {
    prompt: string;
    model?: string;
    aspect_ratio?: string;
    image_size?: string;
  },
  ctx: Ctx,
): Promise<ImageGenerationResult> {
  if (!args.prompt || !args.prompt.trim()) throw new Error("prompt is required");
  return generateImage(ctx, modelId(args.model), [{ text: args.prompt }], args.aspect_ratio, args.image_size);
}

/**
 * Edit (or re-style) an image with a text prompt. `image` is the input file's bytes in base64
 * (standard alphabet; a `data:` URI prefix is accepted), `mime_type` defaults to `image/png`
 * (use `image/jpeg`, `image/gif` or `image/webp` for those). Returns the edited image(s) inline
 * as base64 plus any text, like `generate`; `model`, `aspect_ratio` and `image_size` mean the
 * same as there.
 */
export async function edit(
  args: {
    image: string;
    prompt: string;
    mime_type?: string;
    model?: string;
    aspect_ratio?: string;
    image_size?: string;
  },
  ctx: Ctx,
): Promise<ImageGenerationResult> {
  if (!args.image || !args.image.trim()) throw new Error("image is required (base64 image bytes)");
  if (!args.prompt || !args.prompt.trim()) throw new Error("prompt is required");
  const imagePart: Part = {
    inlineData: { mimeType: args.mime_type || "image/png", data: base64Body(args.image) },
  };
  return generateImage(
    ctx,
    modelId(args.model),
    [imagePart, { text: args.prompt }],
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
