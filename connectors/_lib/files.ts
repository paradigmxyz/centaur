// File helpers shared by connectors: the files capability, output paths,
// downloads and multipart bodies that reference files without reading them.
import type { BodyPart, Ctx, Files, RequestInit } from "../centaur";

/** A file a connector function saved for the caller. */
export interface SavedFile {
  /** Absolute path in the sandbox. */
  path: string;
  bytes: number;
  mime_type?: string;
}

/** `ctx.files`, or a clear error when the runner has no files capability. */
export function requireFiles(ctx: Ctx, what: string): Files {
  if (!ctx.files) {
    throw new Error(`${what} reads or writes files, which needs the files capability: call it from a Centaur sandbox`);
  }
  return ctx.files;
}

/** A file name safe to use as one path segment. */
export function safeFilename(name: string, fallback = "file"): string {
  const cleaned = name
    .replace(/[\u0000-\u001f\u007f/\\]+/g, "_")
    .replace(/^\.+/, "")
    .trim()
    .slice(0, 200);
  return cleaned || fallback;
}

/** `requested`, or `downloads/<connector>/<filename>` under the files root. */
export function outputPath(requested: string | undefined, connector: string, filename: string): string {
  return requested || `downloads/${connector}/${safeFilename(filename)}`;
}

/**
 * The filename from a Content-Disposition header (`filename*=UTF-8''...` or `filename="..."`),
 * if any.
 */
export function dispositionFilename(header: string | undefined): string | undefined {
  if (!header) return undefined;
  const extended = /filename\*\s*=\s*(?:UTF-8|utf-8)''([^;]+)/.exec(header);
  if (extended) {
    try {
      return decodeURIComponent(extended[1].trim().replace(/^"|"$/g, ""));
    } catch {
      // fall through to the plain parameter
    }
  }
  const plain = /filename\s*=\s*("([^"]*)"|[^;]+)/.exec(header);
  return plain ? (plain[2] ?? plain[1]).trim() : undefined;
}

/** How long a file download may take by default; `init.timeout_ms` overrides it. */
export const DOWNLOAD_TIMEOUT_MS: number = 5 * 60 * 1000;

/**
 * Download `url` to `path`; throws `<label> returned HTTP <status>: <body>` on a non-2xx
 * response. Files can be large, so the transfer may take up to `DOWNLOAD_TIMEOUT_MS` unless
 * `init.timeout_ms` says otherwise.
 */
export async function downloadTo(
  ctx: Ctx,
  url: string,
  path: string,
  label: string,
  init?: RequestInit,
): Promise<SavedFile> {
  const files = requireFiles(ctx, label);
  const res = await files.download(url, path, { timeout_ms: DOWNLOAD_TIMEOUT_MS, ...init });
  if (!res.ok || !res.path) {
    throw new Error(`${label} returned HTTP ${res.status}: ${(res.body ?? "").slice(0, 1000)}`);
  }
  const mime = res.headers["content-type"];
  return { path: res.path, bytes: res.bytes ?? 0, ...(mime ? { mime_type: mime } : {}) };
}

/** Save bytes produced in the realm (for example decoded from an API's JSON). */
export async function saveBytes(
  ctx: Ctx,
  path: string,
  data: Uint8Array | string,
  label: string,
  mime_type?: string,
): Promise<SavedFile> {
  const info = await requireFiles(ctx, label).write(path, data);
  return { path: info.path, bytes: info.bytes, ...(mime_type ? { mime_type } : {}) };
}

/** One part of a multipart body. */
export interface MultipartPart {
  /** Form field name; sets `Content-Disposition: form-data` (with `filename` when given). */
  name?: string;
  filename?: string;
  content_type?: string;
  /** Extra or replacement part headers (for `multipart/related`). */
  headers?: Record<string, string>;
  /** Inline content, or `{ file: path }` read by the runtime. */
  body: string | Uint8Array | { file: string };
}

/**
 * A multipart body whose file parts stay on disk until the runtime sends them. Pass `body` as
 * `RequestInit.body` and `content_type` as the Content-Type header.
 */
export function multipart(
  parts: MultipartPart[],
  subtype: "form-data" | "related" | "mixed" = "form-data",
): { body: BodyPart[]; content_type: string } {
  const boundary = `centaur-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  const body: BodyPart[] = [];
  for (const part of parts) {
    const headers: Record<string, string> = {};
    if (part.name !== undefined) {
      const quote = (value: string): string => value.replace(/[\r\n"]/g, (c) => encodeURIComponent(c));
      headers["Content-Disposition"] = `form-data; name="${quote(part.name)}"` +
        (part.filename !== undefined ? `; filename="${quote(part.filename)}"` : "");
    }
    if (part.content_type) headers["Content-Type"] = part.content_type;
    Object.assign(headers, part.headers ?? {});
    const head = Object.entries(headers).map(([key, value]) => `${key}: ${value}\r\n`).join("");
    body.push(`--${boundary}\r\n${head}\r\n`);
    body.push(typeof part.body === "object" && !(part.body instanceof Uint8Array) ? { file: part.body.file } : part.body);
    body.push("\r\n");
  }
  body.push(`--${boundary}--\r\n`);
  return { body, content_type: `multipart/${subtype}; boundary=${boundary}` };
}

/** File extension (without the dot) for common MIME types, for naming saved files. */
export function extensionFor(mime_type: string | undefined): string {
  const type = (mime_type ?? "").split(";")[0].trim().toLowerCase();
  const known: Record<string, string> = {
    "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp", "image/gif": "gif",
    "video/mp4": "mp4", "application/pdf": "pdf", "text/plain": "txt", "text/csv": "csv",
    "text/html": "html", "application/json": "json", "application/zip": "zip",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "docx",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet": "xlsx",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation": "pptx",
  };
  return known[type] ?? "bin";
}
