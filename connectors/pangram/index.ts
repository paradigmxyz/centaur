import type { Ctx } from "../centaur";
import { readJson } from "../_lib/http";

// Results are Pangram's own objects, passed through. The interfaces list the
// fields worth knowing; Pangram may send more.

const TEXT_BASE = "https://text.external-api.pangram.com";
const PLAGIARISM_BASE = "https://plagiarism.api.pangram.com";
const DEFAULT_MODEL = "default";

/** The model selectors available to this API key, in preference order. */
export interface ModelsResponse {
  models: string[];
  [field: string]: unknown;
}

/** The response to submitting a detection task. */
export interface TaskSubmission {
  task_id: string;
  stage?: string;
  [field: string]: unknown;
}

/**
 * A detection task. `stage` advances through preprocessing and analysis to `STAGE_SUCCESS` or
 * `STAGE_FAILED`; the fractions are fractions of the text classified in each way.
 */
export interface DetectionTask {
  task_id?: string;
  stage: string;
  prediction_short?: string;
  prediction?: string;
  fraction_ai?: number;
  fraction_ai_assisted?: number;
  fraction_human?: number;
  [field: string]: unknown;
}

/** A plagiarism check result; `plagiarism_detected` is the headline verdict. */
export interface PlagiarismReport {
  plagiarism_detected?: boolean;
  [field: string]: unknown;
}

async function request(
  ctx: Ctx,
  url: string,
  init?: { method?: string; body?: unknown },
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.PANGRAM_API_KEY) headers["x-api-key"] = ctx.env.PANGRAM_API_KEY;
  if (init?.body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(url, {
    method: init?.method ?? "GET",
    headers,
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
  });
  return readJson(res, "Pangram");
}

/**
 * List the AI-detection model selectors available to the API key, in preference order. Pass one
 * as `model` to `create_task`; `"default"` selects Pangram's current default.
 */
export async function list_models(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<ModelsResponse> {
  return request(ctx, `${TEXT_BASE}/models`);
}

/**
 * Start an asynchronous AI-detection task for `text` and return its `task_id` and initial stage.
 * `model` is a selector from `list_models` (default `"default"`). `public_dashboard_link` asks for
 * a shareable dashboard URL. Poll with `get_task`; the task is done at `STAGE_SUCCESS` or
 * `STAGE_FAILED`.
 */
export async function create_task(
  args: { text: string; model?: string; public_dashboard_link?: boolean },
  ctx: Ctx,
): Promise<TaskSubmission> {
  return request(ctx, `${TEXT_BASE}/task`, {
    method: "POST",
    body: {
      text: args.text,
      model: args.model ?? DEFAULT_MODEL,
      public_dashboard_link: args.public_dashboard_link ?? false,
    },
  });
}

/**
 * Return the current status or result of an AI-detection task by `task_id`. Poll this after
 * `create_task`; `stage` is terminal when it is `STAGE_SUCCESS` or `STAGE_FAILED`, and a successful
 * result includes `prediction_short`, `prediction` and `fraction_ai`/`fraction_human`.
 */
export async function get_task(args: { task_id: string }, ctx: Ctx): Promise<DetectionTask> {
  return request(ctx, `${TEXT_BASE}/task/${encodeURIComponent(args.task_id)}`);
}

/**
 * Check `text` for potential plagiarism against online content. Returns Pangram's plagiarism
 * report, whose headline field is `plagiarism_detected`.
 */
export async function check_plagiarism(
  args: { text: string },
  ctx: Ctx,
): Promise<PlagiarismReport> {
  return request(ctx, PLAGIARISM_BASE, { method: "POST", body: { text: args.text } });
}

