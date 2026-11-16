import type { Ctx, Response } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Google Slides v1 resources as the API sends them.
// Authentication is injected on the wire by iron-proxy's oauth_token transform;
// `GOOGLE_ACCESS_TOKEN` is a local-testing fallback only.

const DEFAULT_SLIDES_BASE = "https://slides.googleapis.com/v1";
const DEFAULT_DRIVE_BASE = "https://www.googleapis.com/drive/v3";

/** A presentation: title, page size, revision and its slides in order. */
export interface Presentation {
  presentationId: string;
  title?: string;
  /** Opaque current revision; pass as `required_revision_id` for guarded writes. */
  revisionId?: string;
  /** Slide size in EMU (1/914400 inch) and points. */
  pageSize?: {
    width?: { magnitude?: number; unit?: string };
    height?: { magnitude?: number; unit?: string };
  };
  slides?: Page[];
  layouts?: Page[];
  masters?: Page[];
  notesMaster?: Page;
  locale?: string;
  [field: string]: unknown;
}

/** A slide (or layout/master page) with its page elements and speaker notes. */
export interface Page {
  objectId: string;
  pageType?: string;
  /** Text elements carry `shape.text.textElements[].textRun.content`. */
  pageElements?: PageElement[];
  /** A slide's speaker notes, as a `notesPage`-typed page. */
  notesPage?: Page;
  [field: string]: unknown;
}

/** One element on a slide: shape, image, table, line, video, or a group of them. */
export interface PageElement {
  objectId?: string;
  size?: Record<string, unknown>;
  transform?: Record<string, unknown>;
  title?: string;
  description?: string;
  shape?: { shapeType?: string; text?: { textElements?: TextElement[] } };
  image?: { contentUrl?: string; sourceUrl?: string };
  table?: { rows?: number; columns?: number; tableRows?: { tableCells?: TableCell[] }[] };
  line?: Record<string, unknown>;
  video?: Record<string, unknown>;
  elementGroup?: { children?: PageElement[] };
  [field: string]: unknown;
}

/** A run of text inside a shape's text body. */
export interface TextElement {
  startIndex?: number;
  endIndex?: number;
  paragraphMarker?: { style?: Record<string, unknown>; bullet?: unknown };
  textRun?: { content?: string; style?: Record<string, unknown> };
  [field: string]: unknown;
}

/** A table cell and its text body. */
export interface TableCell {
  location?: { rowIndex?: number; columnIndex?: number };
  text?: { textElements?: TextElement[] };
  [field: string]: unknown;
}

interface RequestOptions {
  method?: string;
  body?: unknown;
}

function slidesBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_SLIDES_API_URL || DEFAULT_SLIDES_BASE;
}

function driveBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_DRIVE_API_URL || DEFAULT_DRIVE_BASE;
}

/** iron-proxy adds the bearer token on the wire; locally the env may hold one. */
function authHeaders(ctx: Ctx): Record<string, string> {
  const headers: Record<string, string> = {};
  if (ctx.env.GOOGLE_ACCESS_TOKEN) headers.authorization = `Bearer ${ctx.env.GOOGLE_ACCESS_TOKEN}`;
  return headers;
}

async function send(ctx: Ctx, url: string, options: RequestOptions = {}): Promise<Response> {
  const headers: Record<string, string> = { accept: "application/json", ...authHeaders(ctx) };
  if (options.body !== undefined) headers["content-type"] = "application/json";
  return ctx.fetch(url, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
}

async function slidesRequest(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<any> {
  return readJson(await send(ctx, slidesBase(ctx) + path, options), "Google Slides");
}

/** Accept a presentation id or a Slides/Drive URL and return the id it contains. */
function presentationId(value: string): string {
  if (!value.startsWith("http://") && !value.startsWith("https://")) return value;
  const path = /\/d\/([a-zA-Z0-9_-]+)/.exec(value);
  if (path) return path[1];
  const byQuery = /[?&]id=([a-zA-Z0-9_-]+)/.exec(value);
  if (byQuery) return byQuery[1];
  throw new Error(`Could not extract a presentation id from URL: ${value}`);
}

/**
 * Create an empty presentation, inside `folder_id` (a folder or shared drive id, via Drive) when
 * given. Returns the presentation id, title and editor URL; use `batch_update` to add slides and
 * text.
 */
export async function create(
  args: { title: string; folder_id?: string },
  ctx: Ctx,
): Promise<{ presentation_id: string; title: string; url: string }> {
  let id: string;
  let title: string;
  if (args.folder_id) {
    const created = await readJson(
      await send(
        ctx,
        `${driveBase(ctx)}/files${query({ fields: "id,name", supportsAllDrives: true })}`,
        {
          body: {
            name: args.title,
            mimeType: "application/vnd.google-apps.presentation",
            parents: [args.folder_id],
          },
        },
      ),
      "Google Drive",
    );
    id = created.id;
    title = created.name ?? args.title;
  } else {
    const presentation = await slidesRequest(ctx, "/presentations", {
      body: { title: args.title },
    });
    id = presentation.presentationId;
    title = presentation.title ?? args.title;
  }
  return { presentation_id: id, title, url: `https://docs.google.com/presentation/d/${id}/edit` };
}

/**
 * Read a presentation: title, page size, revision and every slide in order with its elements,
 * object ids, shape text runs and speaker notes. Object ids are what `batch_update` requests
 * address.
 */
export async function get_presentation(
  args: { presentation_id: string },
  ctx: Ctx,
): Promise<{ presentation: Presentation }> {
  const id = presentationId(args.presentation_id);
  return { presentation: await slidesRequest(ctx, `/presentations/${encodeURIComponent(id)}`) };
}

/**
 * Send raw Slides `requests` (createSlide, createShape, insertText, updateTextStyle,
 * createImage, createTable, deleteObject, ... see the Slides batchUpdate reference) to one
 * presentation. They apply in order; `required_revision_id` (see `get_presentation`) makes Google
 * reject the whole batch if the deck changed since. Returns the presentation id and one reply per
 * request.
 */
export async function batch_update(
  args: { presentation_id: string; requests: unknown[]; required_revision_id?: string },
  ctx: Ctx,
): Promise<{ presentationId: string; replies: unknown[] }> {
  const id = presentationId(args.presentation_id);
  const body: Record<string, unknown> = { requests: args.requests };
  if (args.required_revision_id) body.writeControl = { requiredRevisionId: args.required_revision_id };
  const result = await slidesRequest(ctx, `/presentations/${encodeURIComponent(id)}:batchUpdate`, {
    body,
  });
  return { presentationId: result.presentationId ?? id, replies: result.replies ?? [] };
}
