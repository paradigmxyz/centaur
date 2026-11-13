import type { Ctx, Response } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Google Drive v3 resources as the API sends them (`camelCase`,
// sizes as decimal strings, times as RFC 3339); fields a request's `fields`
// mask does not ask for are absent. Authentication is injected on the wire by
// iron-proxy's oauth_token transform; `GOOGLE_ACCESS_TOKEN` is a local-testing
// fallback only.

const DEFAULT_BASE = "https://www.googleapis.com/drive/v3";
const DEFAULT_UPLOAD_BASE = "https://www.googleapis.com/upload/drive/v3";

const LIST_FIELDS =
  "incompleteSearch,files(id,name,mimeType,size,modifiedTime,webViewLink,parents),nextPageToken";
const FILE_FIELDS = "id,name,mimeType,size,modifiedTime,webViewLink,parents,owners";
const REVISION_FIELDS =
  "id,mimeType,modifiedTime,keepForever,published,publishAuto,publishedOutsideDomain," +
  "publishedLink,originalFilename,md5Checksum,size,lastModifyingUser(displayName,emailAddress," +
  "photoLink,me,permissionId),exportLinks";

/** Export format name to the Google Workspace MIME type Drive exports as. */
const EXPORT_FORMATS: Record<string, string> = {
  txt: "text/plain",
  pdf: "application/pdf",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  html: "text/html",
  csv: "text/csv",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  md: "text/markdown",
};

/** A user reference, such as a file owner or a revision's last modifier. */
export interface User {
  displayName?: string;
  emailAddress?: string;
  photoLink?: string;
  me?: boolean;
  permissionId?: string;
  [field: string]: unknown;
}

/** A Drive file, folder or shared drive item. */
export interface DriveFile {
  id: string;
  name: string;
  mimeType: string;
  /** Size in bytes, as Drive's decimal string. Absent for Google-native files. */
  size?: string;
  /** RFC 3339, for example `2024-05-01T12:34:56.789Z`. */
  modifiedTime?: string;
  webViewLink?: string;
  parents?: string[];
  owners?: User[];
  [field: string]: unknown;
}

/** A shared drive the caller is a member of. */
export interface SharedDrive {
  id: string;
  name?: string;
  [field: string]: unknown;
}

/** A historical revision of a file. */
export interface Revision {
  id: string;
  mimeType?: string;
  modifiedTime?: string;
  keepForever?: boolean;
  published?: boolean;
  publishAuto?: boolean;
  publishedOutsideDomain?: boolean;
  publishedLink?: string;
  /** Present on uploaded binary files, absent on Google-native files. */
  originalFilename?: string;
  md5Checksum?: string;
  size?: string;
  lastModifyingUser?: User;
  /** MIME type to export URL for Google Docs, Sheets and Slides revisions. */
  exportLinks?: Record<string, string>;
  [field: string]: unknown;
}

/** A permission grant on a file. */
export interface Permission {
  id: string;
  type?: string;
  role?: string;
  emailAddress?: string;
  displayName?: string;
  [field: string]: unknown;
}

/** Bytes of a file or export, base64-encoded for transport. */
export interface Download {
  name: string;
  mime_type: string;
  web_view_link?: string;
  /** Decoded size in bytes; the runtime caps a result at 10 MiB. */
  size: number;
  /** Standard-alphabet base64 of the bytes. */
  content_base64: string;
}

/** The outcome of `setup_channel_permissions`. */
export interface ChannelPermissionsResult {
  shared_with: string[];
  share_errors: { email: string; error: string }[];
  new_owner: string | null;
  ownership_error: string | null;
}

interface RequestOptions {
  method?: string;
  body?: unknown;
  headers?: Record<string, string>;
}

function base(ctx: Ctx): string {
  return ctx.env.GOOGLE_DRIVE_API_URL || DEFAULT_BASE;
}

function uploadBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_DRIVE_UPLOAD_URL || DEFAULT_UPLOAD_BASE;
}

/** iron-proxy adds the bearer token on the wire; locally the env may hold one. */
function authHeaders(ctx: Ctx): Record<string, string> {
  const headers: Record<string, string> = {};
  if (ctx.env.GOOGLE_ACCESS_TOKEN) headers.authorization = `Bearer ${ctx.env.GOOGLE_ACCESS_TOKEN}`;
  return headers;
}

async function send(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<Response> {
  const headers: Record<string, string> = { accept: "application/json", ...authHeaders(ctx) };
  if (options.body !== undefined) headers["content-type"] = "application/json";
  return ctx.fetch(base(ctx) + path, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers: { ...headers, ...options.headers },
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
}

async function request(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<any> {
  return readJson(await send(ctx, path, options), "Google Drive");
}

async function requestEmpty(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<void> {
  const res = await send(ctx, path, options);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Google Drive returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  }
}

/** Accept a Drive id or a Drive/Docs URL and return the id it contains. */
function driveId(value: string): string {
  if (!value.startsWith("http://") && !value.startsWith("https://")) return value;
  const byPath = /\/d\/([a-zA-Z0-9_-]+)/.exec(value);
  if (byPath) return byPath[1];
  const byFolder = /\/folders\/([a-zA-Z0-9_-]+)/.exec(value);
  if (byFolder) return byFolder[1];
  const byQuery = /[?&]id=([a-zA-Z0-9_-]+)/.exec(value);
  if (byQuery) return byQuery[1];
  throw new Error(`Could not extract a Drive file id from URL: ${value}`);
}

/** A Drive query string literal: single quotes and backslashes escaped. */
function literal(value: string): string {
  return `'${value.replace(/\\/g, "\\\\").replace(/'/g, "\\'")}'`;
}

function exportMimeType(exportFormat: string): string {
  const mimeType = EXPORT_FORMATS[exportFormat];
  if (!mimeType) {
    throw new Error(
      `Unsupported export format ${exportFormat}. Supported: ${Object.keys(EXPORT_FORMATS).join(", ")}`,
    );
  }
  return mimeType;
}

/** UTF-8 bytes of a string, without a TextEncoder in the connector realm. */
function utf8(ctx: Ctx, text: string): Uint8Array {
  return ctx.base64.decode(ctx.base64.encode(text));
}

function concat(parts: Uint8Array[]): Uint8Array {
  let length = 0;
  for (const part of parts) length += part.length;
  const out = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

async function downloadUrl(ctx: Ctx, url: string, label: string): Promise<Uint8Array> {
  const res = await ctx.fetch(url, { headers: authHeaders(ctx) });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`${label} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  }
  return res.bytes();
}

async function fileMetadata(ctx: Ctx, fileId: string, fields: string): Promise<any> {
  return request(
    ctx,
    `/files/${encodeURIComponent(driveId(fileId))}${query({ fields, supportsAllDrives: true })}`,
  );
}

async function fileBytes(
  ctx: Ctx,
  fileId: string,
  fields: string,
): Promise<{ metadata: any; bytes: Uint8Array }> {
  const metadata = await fileMetadata(ctx, fileId, fields);
  const res = await ctx.fetch(
    `${base(ctx)}/files/${encodeURIComponent(driveId(fileId))}${query({ alt: "media", supportsAllDrives: true })}`,
    { headers: authHeaders(ctx) },
  );
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Google Drive download returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  }
  return { metadata, bytes: await res.bytes() };
}

function downloadResult(
  metadata: { name?: string; mimeType?: string; webViewLink?: string },
  mimeType: string,
  bytes: Uint8Array,
  ctx: Ctx,
): Download {
  return {
    name: metadata.name || "",
    mime_type: metadata.mimeType || mimeType,
    web_view_link: metadata.webViewLink,
    size: bytes.length,
    content_base64: ctx.base64.encode(bytes),
  };
}

/**
 * List or search Drive files, newest relevance first, across My Drive and every shared drive the
 * caller can reach. `query` matches file names, or full text (file contents and metadata) with
 * `full_text`; `folder_id` lists one folder's children (a shared drive id lists its top level,
 * see `list_drives`); `file_type` is a MIME type, or a prefix ending in `/` such as `image/`.
 * Trashed files are excluded unless `include_trashed`. Pass a previous `next_page_token` to
 * continue; `max_results` is the page size (default 50, at most 1000). `incomplete_search` is
 * true when Drive could not search every drive, so results may be missing files.
 */
export async function list_files(
  args: {
    query?: string;
    folder_id?: string;
    max_results?: number;
    file_type?: string;
    full_text?: boolean;
    include_trashed?: boolean;
    page_token?: string;
  },
  ctx: Ctx,
): Promise<{ files: DriveFile[]; next_page_token?: string; incomplete_search: boolean }> {
  const parts: string[] = [];
  if (args.query) {
    parts.push(`${args.full_text ? "fullText" : "name"} contains ${literal(args.query)}`);
  }
  if (args.folder_id) parts.push(`${literal(driveId(args.folder_id))} in parents`);
  if (args.file_type) {
    parts.push(
      args.file_type.endsWith("/")
        ? `mimeType contains ${literal(args.file_type)}`
        : `mimeType = ${literal(args.file_type)}`,
    );
  }
  if (!args.include_trashed) parts.push("trashed = false");

  const body = await request(
    ctx,
    `/files${query({
      pageSize: args.max_results ?? 50,
      fields: LIST_FIELDS,
      q: parts.length ? parts.join(" and ") : undefined,
      corpora: "allDrives",
      includeItemsFromAllDrives: true,
      supportsAllDrives: true,
      pageToken: args.page_token,
    })}`,
  );
  return {
    files: body.files ?? [],
    next_page_token: body.nextPageToken,
    incomplete_search: body.incompleteSearch === true,
  };
}

/**
 * List the shared drives the caller is a member of, following pages up to `max_results`
 * (default 100). Use a drive's `id` as `folder_id` in `list_files` to browse its top level.
 */
export async function list_drives(
  args: { max_results?: number },
  ctx: Ctx,
): Promise<{ drives: SharedDrive[] }> {
  const max = args.max_results ?? 100;
  if (max < 1) throw new Error("max_results must be at least 1");
  const drives: SharedDrive[] = [];
  let pageToken: string | undefined;
  while (drives.length < max) {
    const body = await request(
      ctx,
      `/drives${query({
        pageSize: Math.min(100, max - drives.length),
        fields: "nextPageToken,drives(id,name)",
        pageToken,
      })}`,
    );
    for (const drive of body.drives ?? []) drives.push(drive);
    pageToken = body.nextPageToken;
    if (!pageToken) break;
  }
  return { drives: drives.slice(0, max) };
}

/**
 * Metadata for one file or folder: name, MIME type, size, modified time, web link, parent ids and
 * owners. Works for files in shared drives.
 */
export async function get_file(args: { file_id: string }, ctx: Ctx): Promise<{ file: DriveFile }> {
  return { file: await fileMetadata(ctx, args.file_id, FILE_FIELDS) };
}

/**
 * Download a binary file's bytes as base64 (`content_base64`). Google Docs, Sheets and Slides have
 * no binary form: use `export_file` for those. The runtime caps an HTTP response at 10 MiB and a
 * result at 10 MiB, so files above roughly 7.5 MiB fail (base64 costs a third more).
 */
export async function download_file(
  args: { file_id: string },
  ctx: Ctx,
): Promise<Download> {
  const { metadata, bytes } = await fileBytes(ctx, args.file_id, "name,mimeType,webViewLink");
  return downloadResult(metadata, "application/octet-stream", bytes, ctx);
}

/**
 * Upload a file from base64 content into My Drive or a folder, optionally converting it to a
 * native Google Sheet (`convert_to_sheets`, for CSV and Excel uploads). `mime_type` defaults to
 * `application/octet-stream`; pass the real type so Drive can preview and convert it. Returns the
 * new file's id, name and web link.
 */
export async function upload_file(
  args: {
    content_base64: string;
    name: string;
    folder_id?: string;
    mime_type?: string;
    convert_to_sheets?: boolean;
  },
  ctx: Ctx,
): Promise<{ id: string; name: string; webViewLink?: string }> {
  const bytes = ctx.base64.decode(args.content_base64);
  const metadata: Record<string, unknown> = { name: args.name };
  if (args.folder_id) metadata.parents = [args.folder_id];
  if (args.convert_to_sheets) metadata.mimeType = "application/vnd.google-apps.spreadsheet";
  const contentType = args.mime_type || "application/octet-stream";
  const boundary = "centaur-code-mode-boundary";
  const body = concat([
    utf8(
      ctx,
      `--${boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n` +
        `${JSON.stringify(metadata)}\r\n--${boundary}\r\nContent-Type: ${contentType}\r\n\r\n`,
    ),
    bytes,
    utf8(ctx, `\r\n--${boundary}--\r\n`),
  ]);

  const res = await ctx.fetch(
    `${uploadBase(ctx)}/files${query({
      uploadType: "multipart",
      supportsAllDrives: true,
      fields: "id,name,webViewLink",
    })}`,
    {
      method: "POST",
      headers: {
        accept: "application/json",
        "content-type": `multipart/related; boundary=${boundary}`,
        ...authHeaders(ctx),
      },
      body,
    },
  );
  return readJson(res, "Google Drive upload");
}

/** Create a folder, optionally inside `parent_id` (a folder or shared drive id). */
export async function create_folder(
  args: { name: string; parent_id?: string },
  ctx: Ctx,
): Promise<{ folder: DriveFile }> {
  const body: Record<string, unknown> = {
    name: args.name,
    mimeType: "application/vnd.google-apps.folder",
  };
  if (args.parent_id) body.parents = [driveId(args.parent_id)];
  const folder = await request(
    ctx,
    `/files${query({ fields: "id,name,mimeType,webViewLink,parents", supportsAllDrives: true })}`,
    { body },
  );
  return { folder };
}

/**
 * List a file's revision history, newest first, following pages up to `max_results` (default 200,
 * page size at most 1000). Works for Google Docs, Sheets and Slides as well as binaries; Google
 * may omit older entries for heavily edited files. Each revision carries `exportLinks` (native
 * files) or an `originalFilename`/`md5Checksum` (binaries).
 */
export async function list_revisions(
  args: { file_id: string; max_results?: number },
  ctx: Ctx,
): Promise<{ revisions: Revision[] }> {
  const max = args.max_results ?? 200;
  if (max < 1) throw new Error("max_results must be at least 1");
  const revisions: Revision[] = [];
  let pageToken: string | undefined;
  while (revisions.length < max) {
    const body = await request(
      ctx,
      `/files/${encodeURIComponent(driveId(args.file_id))}/revisions${query({
        pageSize: Math.min(1000, max - revisions.length),
        fields: `nextPageToken,revisions(${REVISION_FIELDS})`,
        pageToken,
      })}`,
    );
    for (const revision of body.revisions ?? []) revisions.push(revision);
    pageToken = body.nextPageToken;
    if (!pageToken) break;
  }
  return { revisions: revisions.slice(0, max) };
}

/** Metadata for one revision, including the export links available for it. */
export async function get_revision(
  args: { file_id: string; revision_id: string },
  ctx: Ctx,
): Promise<{ revision: Revision }> {
  return {
    revision: await request(
      ctx,
      `/files/${encodeURIComponent(driveId(args.file_id))}/revisions/${encodeURIComponent(args.revision_id)}${query({ fields: REVISION_FIELDS })}`,
    ),
  };
}

/**
 * Export a Google Doc, Sheet or Slides deck to another format, returned as base64
 * (`content_base64`). `export_format` is one of `txt`, `pdf`, `docx`, `html`, `csv`, `xlsx`,
 * `pptx`, `md`; the format must suit the file type (csv/xlsx for Sheets, pptx for Slides, docx/pdf
 * for Docs). The runtime's 10 MiB result cap means exports above roughly 7.5 MiB fail.
 */
export async function export_file(
  args: { file_id: string; export_format?: string },
  ctx: Ctx,
): Promise<Download> {
  const exportFormat = args.export_format ?? "txt";
  const mimeType = exportMimeType(exportFormat);
  const metadata = await fileMetadata(ctx, args.file_id, "name,mimeType,webViewLink");
  const bytes = await downloadUrl(
    ctx,
    `${base(ctx)}/files/${encodeURIComponent(driveId(args.file_id))}/export${query({ mimeType })}`,
    "Google Drive export",
  );
  const result = downloadResult(metadata, mimeType, bytes, ctx);
  return { ...result, name: `${result.name.replace(/\.[^./]*$/, "")}.${exportFormat}` };
}

/**
 * Export an earlier revision of a Google Doc, Sheet or Slides deck as base64 (`content_base64`),
 * using the revision's own `exportLinks` (`export_format`: txt, pdf, docx, html, csv, xlsx, pptx,
 * md). Throws when that revision cannot be exported in the requested format; see `get_revision`
 * for the formats it offers.
 */
export async function export_revision(
  args: { file_id: string; revision_id: string; export_format?: string },
  ctx: Ctx,
): Promise<Download> {
  const exportFormat = args.export_format ?? "pdf";
  const mimeType = exportMimeType(exportFormat);
  const metadata = await fileMetadata(ctx, args.file_id, "name,mimeType,webViewLink");
  const revision: Revision = await request(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/revisions/${encodeURIComponent(args.revision_id)}${query({ fields: REVISION_FIELDS })}`,
  );
  const exportUrl = (revision.exportLinks || {})[mimeType];
  if (!exportUrl) {
    const links = revision.exportLinks || {};
    const available = Object.keys(EXPORT_FORMATS).filter(
      (name) => links[EXPORT_FORMATS[name]] !== undefined,
    );
    throw new Error(
      `Revision ${args.revision_id} cannot be exported as ${exportFormat}. Available formats: ${
        available.join(", ") || "none"
      }`,
    );
  }
  // Export links are pre-signed: they carry no bearer token, and iron-proxy only
  // injects OAuth for the googleapis.com hosts.
  const bytes = await downloadUrl(ctx, exportUrl, "Google Drive revision export");
  const result = downloadResult(metadata, mimeType, bytes, ctx);
  return { ...result, name: `${result.name.replace(/\.[^./]*$/, "")}-revision-${args.revision_id}.${exportFormat}` };
}

/**
 * Download an earlier revision of a binary (non-Google-native) file as base64
 * (`content_base64`). Native Docs, Sheets and Slides have no binary revision: use
 * `export_revision` for those.
 */
export async function download_revision(
  args: { file_id: string; revision_id: string },
  ctx: Ctx,
): Promise<Download> {
  const metadata = await fileMetadata(ctx, args.file_id, "name,mimeType,webViewLink");
  const revision: Revision = await request(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/revisions/${encodeURIComponent(args.revision_id)}${query({ fields: REVISION_FIELDS })}`,
  );
  const mimeType = revision.mimeType || metadata.mimeType || "";
  if (mimeType.startsWith("application/vnd.google-apps.")) {
    throw new Error(
      "Google Docs, Sheets and Slides revisions must be downloaded with export_revision",
    );
  }
  const bytes = await ctx
    .fetch(
      `${base(ctx)}/files/${encodeURIComponent(driveId(args.file_id))}/revisions/${encodeURIComponent(args.revision_id)}${query({ alt: "media" })}`,
      { headers: authHeaders(ctx) },
    )
    .then(async (res) => {
      if (!res.ok) {
        const text = await res.text();
        throw new Error(
          `Google Drive revision download returned HTTP ${res.status}: ${text.slice(0, 1000)}`,
        );
      }
      return res.bytes();
    });
  const stem = (revision.originalFilename || metadata.name || args.file_id).replace(/\.[^./]*$/, "");
  const extension = (revision.originalFilename || metadata.name || "").match(/\.[^./]*$/)?.[0] || "";
  const result = downloadResult(metadata, mimeType, bytes, ctx);
  return { ...result, name: `${stem}-revision-${args.revision_id}${extension}` };
}

/**
 * List who a file is shared with: permission id, type (`user`, `group`, `domain`, `anyone`), role
 * (`owner`, `organizer`, `fileOrganizer`, `writer`, `commenter`, `reader`), email and display name.
 */
export async function list_permissions(
  args: { file_id: string },
  ctx: Ctx,
): Promise<{ permissions: Permission[] }> {
  const body = await request(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/permissions${query({
      fields: "permissions(id,type,role,emailAddress,displayName)",
      supportsAllDrives: true,
    })}`,
  );
  return { permissions: body.permissions ?? [] };
}

/**
 * Grant a person access to a file by email. `role` is `reader`, `commenter`, `writer` or `owner`
 * (use `transfer_ownership` for owner); `send_notification` emails them about the share.
 */
export async function share(
  args: { file_id: string; email: string; role?: string; send_notification?: boolean },
  ctx: Ctx,
): Promise<{ permission: Permission }> {
  const permission = await request(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/permissions${query({
      sendNotificationEmail: args.send_notification === true,
      supportsAllDrives: true,
      fields: "id,type,role,emailAddress,displayName",
    })}`,
    { body: { type: "user", role: args.role ?? "writer", emailAddress: args.email } },
  );
  return { permission };
}

/**
 * Make `new_owner_email` the owner of a file, adding them as a writer first when they have no
 * access yet. Both accounts must be in the same Workspace domain; the current owner is downgraded
 * to editor.
 */
export async function transfer_ownership(
  args: { file_id: string; new_owner_email: string },
  ctx: Ctx,
): Promise<{ permission: Permission }> {
  const { permissions } = await list_permissions({ file_id: args.file_id }, ctx);
  const wanted = args.new_owner_email.toLowerCase();
  let permissionId: string | undefined = permissions.find(
    (permission) => (permission.emailAddress || "").toLowerCase() === wanted,
  )?.id;
  if (!permissionId) {
    permissionId = (await share({ file_id: args.file_id, email: args.new_owner_email }, ctx))
      .permission.id;
  }
  const permission = await request(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/permissions/${encodeURIComponent(permissionId)}${query({
      transferOwnership: true,
      supportsAllDrives: true,
      fields: "id,type,role,emailAddress,displayName",
    })}`,
    { method: "PATCH", body: { role: "owner" } },
  );
  return { permission };
}

/**
 * Remove a person's permission from a file, looked up by email. Returns `removed: false` when no
 * permission for that email exists.
 */
export async function remove_permission(
  args: { file_id: string; email: string },
  ctx: Ctx,
): Promise<{ removed: boolean }> {
  const { permissions } = await list_permissions({ file_id: args.file_id }, ctx);
  const wanted = args.email.toLowerCase();
  const permissionId = permissions.find(
    (permission) => (permission.emailAddress || "").toLowerCase() === wanted,
  )?.id;
  if (!permissionId) return { removed: false };
  await requestEmpty(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/permissions/${encodeURIComponent(permissionId)}${query({ supportsAllDrives: true })}`,
    { method: "DELETE" },
  );
  return { removed: true };
}

/**
 * Apply a Drive label to one file. `label_id` comes from the file's label schema; a file can hold
 * one label of each kind, and applying a second replaces its field values. Returns the labels
 * Drive reports as modified.
 */
export async function add_label(
  args: { file_id: string; label_id: string },
  ctx: Ctx,
): Promise<{ modifiedLabels: unknown[]; kind?: string }> {
  const body = await request(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/modifyLabels`,
    { body: { labelModifications: [{ labelId: args.label_id }] } },
  );
  return { modifiedLabels: body.modifiedLabels ?? [], kind: body.kind };
}

/** Remove a Drive label from one file. Returns the labels Drive reports as modified. */
export async function remove_label(
  args: { file_id: string; label_id: string },
  ctx: Ctx,
): Promise<{ modifiedLabels: unknown[]; kind?: string }> {
  const body = await request(
    ctx,
    `/files/${encodeURIComponent(driveId(args.file_id))}/modifyLabels`,
    { body: { labelModifications: [{ labelId: args.label_id, removeLabel: true }] } },
  );
  return { modifiedLabels: body.modifiedLabels ?? [], kind: body.kind };
}

/**
 * Apply a label to every file under a folder or shared drive, recursing into subfolders unless
 * `recursive` is false. Returns the labeled and failed files; each file is one modifying request,
 * so folders with many files can exhaust the per-cell call limit or run long.
 */
export async function label_folder(
  args: { folder_id: string; label_id: string; recursive?: boolean },
  ctx: Ctx,
): Promise<{
  labeled: { id: string; name: string }[];
  failed: { id: string; name: string; error: string }[];
}> {
  const recursive = args.recursive !== false;
  const labeled: { id: string; name: string }[] = [];
  const failed: { id: string; name: string; error: string }[] = [];
  const pending = [driveId(args.folder_id)];
  while (pending.length > 0) {
    const folderId = pending.shift() as string;
    let pageToken: string | undefined;
    do {
      const body = await request(
        ctx,
        `/files${query({
          q: `${literal(folderId)} in parents and trashed = false`,
          pageSize: 100,
          fields: "nextPageToken,files(id,name,mimeType)",
          includeItemsFromAllDrives: true,
          supportsAllDrives: true,
          pageToken,
        })}`,
      );
      for (const file of body.files ?? []) {
        if (file.mimeType === "application/vnd.google-apps.folder") {
          if (recursive) pending.push(file.id);
          continue;
        }
        try {
          await add_label({ file_id: file.id, label_id: args.label_id }, ctx);
          labeled.push({ id: file.id, name: file.name });
        } catch (error) {
          failed.push({ id: file.id, name: file.name, error: String(error) });
        }
      }
      pageToken = body.nextPageToken;
    } while (pageToken);
  }
  return { labeled, failed };
}

/**
 * Share a file with every channel member as writer and optionally transfer ownership to the
 * requester. Per-address failures are collected instead of thrown, so one bad email does not stop
 * the rest. Returns who was shared with, share errors, the new owner and any ownership error.
 */
export async function setup_channel_permissions(
  args: { file_id: string; channel_member_emails: string[]; requester_email?: string },
  ctx: Ctx,
): Promise<ChannelPermissionsResult> {
  const result: ChannelPermissionsResult = {
    shared_with: [],
    share_errors: [],
    new_owner: null,
    ownership_error: null,
  };
  for (const email of args.channel_member_emails) {
    try {
      await share(
        { file_id: args.file_id, email, role: "writer", send_notification: false },
        ctx,
      );
      result.shared_with.push(email);
    } catch (error) {
      result.share_errors.push({ email, error: String(error) });
    }
  }
  if (args.requester_email) {
    try {
      await transfer_ownership(
        { file_id: args.file_id, new_owner_email: args.requester_email },
        ctx,
      );
      result.new_owner = args.requester_email;
    } catch (error) {
      result.ownership_error = String(error);
    }
  }
  return result;
}
