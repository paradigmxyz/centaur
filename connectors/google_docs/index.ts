import type { Ctx, Response } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Google Docs v1 and Drive v3 resources as the APIs send them.
// Authentication is injected on the wire by iron-proxy's oauth_token transform;
// `GOOGLE_ACCESS_TOKEN` is a local-testing fallback only.

const DEFAULT_DOCS_BASE = "https://docs.googleapis.com/v1";
const DEFAULT_DRIVE_BASE = "https://www.googleapis.com/drive/v3";
const DEFAULT_BULLET_PRESET = "BULLET_DISC_CIRCLE_SQUARE";
const BULLET_PRESETS = [
  "BULLET_ARROW3D_CIRCLE_SQUARE",
  "BULLET_ARROW_DIAMOND_DISC",
  "BULLET_CHECKBOX",
  "BULLET_DIAMOND_CIRCLE_SQUARE",
  "BULLET_DIAMONDX_ARROW3D_SQUARE",
  "BULLET_DIAMONDX_HOLLOWDIAMOND_SQUARE",
  "BULLET_DISC_CIRCLE_SQUARE",
  "BULLET_LEFTTRIANGLE_DIAMOND_DISC",
  "BULLET_STAR_CIRCLE_SQUARE",
  "NUMBERED_DECIMAL_ALPHA_ROMAN",
  "NUMBERED_DECIMAL_ALPHA_ROMAN_PARENS",
  "NUMBERED_DECIMAL_NESTED",
  "NUMBERED_UPPERALPHA_ALPHA_ROMAN",
  "NUMBERED_UPPERROMAN_UPPERALPHA_DECIMAL",
  "NUMBERED_ZERODECIMAL_ALPHA_ROMAN",
];
const COMMENT_FIELDS =
  "id,content,htmlContent,anchor,quotedFileContent,resolved,deleted,createdTime,modifiedTime," +
  "assigneeEmailAddress,mentionedEmailAddresses,author(displayName,photoLink,me)," +
  "replies(id,content,htmlContent,action,deleted,createdTime,modifiedTime,assigneeEmailAddress," +
  "mentionedEmailAddresses,author(displayName,photoLink,me))";

/** A Google Doc, including its body and (with `include_tabs`) every tab. */
export interface Document {
  documentId: string;
  title?: string;
  /** Opaque current revision; pass as `expected_revision_id` for guarded writes. */
  revisionId?: string;
  body?: { content?: StructuralElement[] };
  tabs?: DocumentTab[];
  [field: string]: unknown;
}

/** A document tab (`documentTab.body`) with its own content and child tabs. */
export interface DocumentTab {
  tabProperties?: { tabId?: string; title?: string; index?: number };
  documentTab?: { body?: { content?: StructuralElement[] }; [field: string]: unknown };
  childTabs?: DocumentTab[];
  [field: string]: unknown;
}

/** One body element: a paragraph, table, table of contents or section break. */
export interface StructuralElement {
  /** UTF-16 code-unit offset; Docs indices are in UTF-16 units. */
  startIndex?: number;
  endIndex?: number;
  paragraph?: { elements?: ParagraphElement[]; bullet?: unknown };
  table?: { tableRows?: { tableCells?: { content?: StructuralElement[] }[] }[] };
  tableOfContents?: { content?: StructuralElement[] };
  sectionBreak?: unknown;
  [field: string]: unknown;
}

/** A paragraph element; `textRun.content` carries the text and its paragraph mark. */
export interface ParagraphElement {
  textRun?: { content?: string; textStyle?: Record<string, unknown> };
  [field: string]: unknown;
}

/** The author of a comment or reply. */
export interface CommentAuthor {
  displayName?: string;
  photoLink?: string;
  me?: boolean;
  [field: string]: unknown;
}

/** A Drive comment on a Doc, with its replies and quoted document text. */
export interface DriveComment {
  id: string;
  content?: string;
  htmlContent?: string;
  anchor?: string;
  quotedFileContent?: { mimeType?: string; value?: string };
  resolved?: boolean;
  deleted?: boolean;
  createdTime?: string;
  modifiedTime?: string;
  assigneeEmailAddress?: string;
  mentionedEmailAddresses?: string[];
  author?: CommentAuthor;
  replies?: DriveCommentReply[];
  [field: string]: unknown;
}

/** A reply on a Drive comment (`action` is `resolve` or `reopen` for resolution replies). */
export interface DriveCommentReply {
  id: string;
  content?: string;
  htmlContent?: string;
  action?: string;
  deleted?: boolean;
  createdTime?: string;
  modifiedTime?: string;
  assigneeEmailAddress?: string;
  mentionedEmailAddresses?: string[];
  author?: CommentAuthor;
  [field: string]: unknown;
}

/** What `bullets` matched and changed, with a before/after preview per paragraph. */
export interface BulletsResult {
  document_id: string;
  match_prefix: string;
  bullet_preset: string;
  matched_paragraphs: number;
  updated_paragraphs: number;
  verified_paragraphs: number;
  already_bulleted_paragraphs: number;
  dry_run: boolean;
  paragraphs: {
    tab_id: string | null;
    paragraph_index: number;
    before: string;
    after: string;
    nesting_level: number;
  }[];
}

interface Paragraph {
  tab_id: string | null;
  paragraph_index: number;
  start_index: number;
  end_index: number;
  text: string;
  has_bullet: boolean;
}

interface BulletMatch extends Paragraph {
  delete_start: number;
  delete_end: number;
  before: string;
  after: string;
  nesting_level: number;
}

interface RequestOptions {
  method?: string;
  body?: unknown;
}

function docsBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_DOCS_API_URL || DEFAULT_DOCS_BASE;
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

async function send(
  ctx: Ctx,
  url: string,
  options: RequestOptions = {},
): Promise<Response> {
  const headers: Record<string, string> = { accept: "application/json", ...authHeaders(ctx) };
  if (options.body !== undefined) headers["content-type"] = "application/json";
  return ctx.fetch(url, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
}

async function docsRequest(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<any> {
  return readJson(await send(ctx, docsBase(ctx) + path, options), "Google Docs");
}

async function driveRequest(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<any> {
  return readJson(await send(ctx, driveBase(ctx) + path, options), "Google Drive");
}

/** Accept a document id or a Docs/Drive URL and return the id it contains. */
function documentId(value: string): string {
  if (!value.startsWith("http://") && !value.startsWith("https://")) return value;
  const path = /\/d\/([a-zA-Z0-9_-]+)/.exec(value);
  if (path) return path[1];
  const byQuery = /[?&]id=([a-zA-Z0-9_-]+)/.exec(value);
  if (byQuery) return byQuery[1];
  throw new Error(`Could not extract a document id from URL: ${value}`);
}

/** UTF-16 length, matching Docs index units (an astral character counts twice). */
function utf16Length(text: string): number {
  let length = 0;
  for (const character of text) {
    const code = character.codePointAt(0) ?? 0;
    length += code > 0xffff ? 2 : 1;
  }
  return length;
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function paragraphText(paragraph: { elements?: ParagraphElement[] }): string {
  let text = "";
  for (const element of paragraph.elements ?? []) text += element.textRun?.content ?? "";
  return text;
}

function collectParagraphsFromContent(
  content: StructuralElement[],
  tabId: string | null,
  paragraphs: Paragraph[],
  counters: Record<string, number>,
): void {
  for (const element of content) {
    if (element.paragraph) {
      const key = tabId ?? "";
      const paragraphIndex = counters[key] ?? 0;
      counters[key] = paragraphIndex + 1;
      paragraphs.push({
        tab_id: tabId,
        paragraph_index: paragraphIndex,
        start_index: element.startIndex ?? 0,
        end_index: element.endIndex ?? 0,
        text: paragraphText(element.paragraph),
        has_bullet: element.paragraph.bullet !== undefined,
      });
    } else if (element.table) {
      for (const row of element.table.tableRows ?? []) {
        for (const cell of row.tableCells ?? []) {
          collectParagraphsFromContent(cell.content ?? [], tabId, paragraphs, counters);
        }
      }
    } else if (element.tableOfContents) {
      collectParagraphsFromContent(
        element.tableOfContents.content ?? [],
        tabId,
        paragraphs,
        counters,
      );
    }
  }
}

function collectTabBody(
  tabs: DocumentTab[],
  into: { tabId: string | null; content: StructuralElement[] }[],
): void {
  for (const tab of tabs) {
    into.push({
      tabId: tab.tabProperties?.tabId ?? null,
      content: tab.documentTab?.body?.content ?? [],
    });
    collectTabBody(tab.childTabs ?? [], into);
  }
}

/** Every body of the document: each tab's body, or the single untabbed body. */
function documentBodies(document: Document): { tabId: string | null; content: StructuralElement[] }[] {
  const bodies: { tabId: string | null; content: StructuralElement[] }[] = [];
  if (document.tabs && document.tabs.length > 0) {
    collectTabBody(document.tabs, bodies);
    return bodies;
  }
  bodies.push({ tabId: null, content: document.body?.content ?? [] });
  return bodies;
}

function documentParagraphs(document: Document): Paragraph[] {
  const paragraphs: Paragraph[] = [];
  const counters: Record<string, number> = {};
  for (const body of documentBodies(document)) {
    collectParagraphsFromContent(body.content, body.tabId, paragraphs, counters);
  }
  return paragraphs;
}

function bodyText(content: StructuralElement[]): string {
  let text = "";
  for (const element of content) {
    if (element.paragraph) {
      text += paragraphText(element.paragraph);
    } else if (element.table) {
      for (const row of element.table.tableRows ?? []) {
        for (const cell of row.tableCells ?? []) text += bodyText(cell.content ?? []);
      }
    } else if (element.tableOfContents) {
      text += bodyText(element.tableOfContents.content ?? []);
    }
  }
  return text;
}

function buildBulletMatches(
  document: Document,
  matchPrefix: string,
  tabId?: string,
): { matches: BulletMatch[]; alreadyBulleted: number } {
  if (!matchPrefix) throw new Error("match_prefix must not be empty");
  const prefix = new RegExp(`^(\\t*)( *)${escapeRegExp(matchPrefix)}`);
  const matches: BulletMatch[] = [];
  let alreadyBulleted = 0;
  for (const paragraph of documentParagraphs(document)) {
    if (tabId && paragraph.tab_id !== tabId) continue;
    const match = prefix.exec(paragraph.text);
    if (!match) continue;
    if (paragraph.has_bullet) {
      alreadyBulleted += 1;
      continue;
    }
    const nestingTabs = match[1];
    const deletePrefix = `${match[2]}${matchPrefix}`;
    const textAfterPrefix = paragraph.text.slice(match[0].length);
    const transformedText = `${nestingTabs}${textAfterPrefix}`;
    if (!transformedText.trim()) continue;
    const deleteStart = paragraph.start_index + utf16Length(nestingTabs);
    matches.push({
      ...paragraph,
      delete_start: deleteStart,
      delete_end: deleteStart + utf16Length(deletePrefix),
      end_index: paragraph.end_index - utf16Length(deletePrefix),
      before: paragraph.text.replace(/\n+$/, ""),
      after: textAfterPrefix.replace(/\n+$/, ""),
      nesting_level: nestingTabs.length,
    });
  }
  return { matches, alreadyBulleted };
}

function checkBulletPreset(bulletPreset: string): void {
  if (!BULLET_PRESETS.includes(bulletPreset)) {
    throw new Error(
      `Unsupported bullet_preset ${bulletPreset}. Expected one of: ${BULLET_PRESETS.join(", ")}`,
    );
  }
}

/**
 * Fetch a Doc's full structure: body content with text runs, paragraph/table indices and styles,
 * plus every tab when `include_tabs` (default true). Indices are UTF-16 offsets to use with
 * `insert_text` and `batch_update`.
 */
export async function get_document(
  args: { document_id: string; include_tabs?: boolean },
  ctx: Ctx,
): Promise<{ document: Document }> {
  const id = documentId(args.document_id);
  const document = await docsRequest(
    ctx,
    `/documents/${encodeURIComponent(id)}${query({ includeTabsContent: args.include_tabs !== false })}`,
  );
  return { document };
}

/**
 * Read a Doc as plain text: paragraph text in document order, including table cells and every tab
 * (tab texts are joined with a newline). Undo/control characters are left as Docs sends them.
 */
export async function get_text(args: { document_id: string }, ctx: Ctx): Promise<{ text: string }> {
  const { document } = await get_document({ document_id: args.document_id }, ctx);
  const parts = documentBodies(document).map((body) => bodyText(body.content));
  return { text: parts.join("\n") };
}

/**
 * List a Doc's comments, oldest first, with quoted document text (`quotedFileContent.value`),
 * author, resolved/deleted flags, mention and assignee emails, and every reply. Paginates to
 * `max_results` (default 100, page size at most 100); `include_deleted` keeps deleted items.
 */
export async function list_comments(
  args: { document_id: string; max_results?: number; include_deleted?: boolean },
  ctx: Ctx,
): Promise<{ comments: DriveComment[] }> {
  const id = documentId(args.document_id);
  const max = args.max_results ?? 100;
  if (max < 1) throw new Error("max_results must be at least 1");
  const comments: DriveComment[] = [];
  let pageToken: string | undefined;
  while (comments.length < max) {
    const body = await driveRequest(
      ctx,
      `/files/${encodeURIComponent(id)}/comments${query({
        pageSize: Math.min(100, max - comments.length),
        includeDeleted: args.include_deleted === true,
        fields: `nextPageToken,comments(${COMMENT_FIELDS})`,
        pageToken,
      })}`,
    );
    for (const comment of body.comments ?? []) comments.push(comment);
    pageToken = body.nextPageToken;
    if (!pageToken) break;
  }
  return { comments: comments.slice(0, max) };
}

/**
 * Create a Doc, optionally with initial `content` inserted at the top and inside `folder_id` (a
 * folder or shared drive id; the file is created through Drive so it lands in that folder).
 * Returns the document id, title and editor URL.
 */
export async function create(
  args: { title: string; content?: string; folder_id?: string },
  ctx: Ctx,
): Promise<{ document_id: string; title: string; url: string }> {
  let id: string;
  let title: string;
  if (args.folder_id) {
    const created = await driveRequest(
      ctx,
      `/files${query({ fields: "id,name", supportsAllDrives: true })}`,
      {
        body: {
          name: args.title,
          mimeType: "application/vnd.google-apps.document",
          parents: [args.folder_id],
        },
      },
    );
    id = created.id;
    title = created.name ?? args.title;
  } else {
    const document = await docsRequest(ctx, "/documents", { body: { title: args.title } });
    id = document.documentId;
    title = document.title ?? args.title;
  }
  if (args.content) {
    await docsRequest(ctx, `/documents/${encodeURIComponent(id)}:batchUpdate`, {
      body: { requests: [{ insertText: { location: { index: 1 }, text: args.content } }] },
    });
  }
  return { document_id: id, title, url: `https://docs.google.com/document/d/${id}/edit` };
}

/**
 * Append text at the end of a Doc's body, or of one tab with `tab_id`. `expected_revision_id` (see
 * `get_document`) makes Google reject the write if the document changed since that revision, so
 * concurrent human edits are not overwritten. Returns the document id and update replies.
 */
export async function append(
  args: { document_id: string; text: string; tab_id?: string; expected_revision_id?: string },
  ctx: Ctx,
): Promise<{ documentId: string; replies: unknown[] }> {
  const { document } = await get_document({ document_id: args.document_id }, ctx);
  const bodies = documentBodies(document);
  const body = args.tab_id
    ? bodies.find((candidate) => candidate.tabId === args.tab_id)
    : bodies[0];
  if (!body) throw new Error(`Document has no tab ${args.tab_id}`);
  const content = body.content;
  const last = content.length > 0 ? content[content.length - 1] : undefined;
  const index = Math.max(1, (last?.endIndex ?? 2) - 1);
  const location: Record<string, unknown> = { index };
  if (body.tabId) location.tabId = body.tabId;
  return batch_update(
    { document_id: args.document_id, requests: [{ insertText: { location, text: args.text } }], required_revision_id: args.expected_revision_id },
    ctx,
  );
}

/**
 * Insert text at a UTF-16 `index` in the document body (1 is the start; the end index comes from
 * `get_document`), or in one tab with `tab_id`. `expected_revision_id` guards against concurrent
 * edits.
 */
export async function insert_text(
  args: {
    document_id: string;
    text: string;
    index: number;
    tab_id?: string;
    expected_revision_id?: string;
  },
  ctx: Ctx,
): Promise<{ documentId: string; replies: unknown[] }> {
  const location: Record<string, unknown> = { index: args.index };
  if (args.tab_id) location.tabId = args.tab_id;
  return batch_update(
    {
      document_id: args.document_id,
      requests: [{ insertText: { location, text: args.text } }],
      required_revision_id: args.expected_revision_id,
    },
    ctx,
  );
}

/**
 * Insert a page break at a UTF-16 `index` in the document body (default 1, the start), or in one
 * tab with `tab_id`.
 */
export async function insert_page_break(
  args: { document_id: string; index?: number; tab_id?: string },
  ctx: Ctx,
): Promise<{ documentId: string; replies: unknown[] }> {
  const location: Record<string, unknown> = { index: args.index ?? 1 };
  if (args.tab_id) location.tabId = args.tab_id;
  return batch_update(
    {
      document_id: args.document_id,
      requests: [{ insertPageBreak: { location } }],
    },
    ctx,
  );
}

/**
 * Send raw Docs `requests` (insertText, deleteContentRange, updateTextStyle, createParagraphBullets,
 * ... see the Docs batchUpdate reference) to one document. They apply in order; `required_revision_id`
 * makes Google reject the whole batch if the document moved on. Returns the document id and replies.
 */
export async function batch_update(
  args: { document_id: string; requests: unknown[]; required_revision_id?: string },
  ctx: Ctx,
): Promise<{ documentId: string; replies: unknown[] }> {
  const id = documentId(args.document_id);
  const body: Record<string, unknown> = { requests: args.requests };
  if (args.required_revision_id) {
    body.writeControl = { requiredRevisionId: args.required_revision_id };
  }
  const result = await docsRequest(ctx, `/documents/${encodeURIComponent(id)}:batchUpdate`, { body });
  return { documentId: result.documentId ?? "", replies: result.replies ?? [] };
}

/**
 * Replace every occurrence of `old_text` in a Doc, case-sensitively unless `match_case` is false.
 * Returns the document id, replies and how many occurrences changed. `expected_revision_id` guards
 * against concurrent edits.
 */
export async function replace_all_text(
  args: {
    document_id: string;
    old_text: string;
    new_text: string;
    match_case?: boolean;
    expected_revision_id?: string;
  },
  ctx: Ctx,
): Promise<{ documentId: string; replies: unknown[]; occurrences_changed: number }> {
  const result = await batch_update(
    {
      document_id: args.document_id,
      requests: [
        {
          replaceAllText: {
            containsText: { text: args.old_text, matchCase: args.match_case !== false },
            replaceText: args.new_text,
          },
        },
      ],
      required_revision_id: args.expected_revision_id,
    },
    ctx,
  );
  let occurrences = 0;
  for (const reply of result.replies as Record<string, any>[]) {
    if (reply.replaceAllText) occurrences = reply.replaceAllText.occurrencesChanged ?? 0;
  }
  return { documentId: result.documentId, replies: result.replies, occurrences_changed: occurrences };
}

/**
 * Turn paragraphs that start with a literal prefix (default `"- "`) into real Docs bullets,
 * keeping tab indentation as nesting level and removing the prefix text. `bullet_preset` is one of
 * the Docs presets such as `BULLET_DISC_CIRCLE_SQUARE` (default), `BULLET_CHECKBOX` or
 * `NUMBERED_DECIMAL_ALPHA_ROMAN`; `tab_id` limits it to one tab. `dry_run` previews matches
 * without writing. Returns match counts, a before/after preview per paragraph, and how many were
 * verified as bulleted afterwards.
 */
export async function bullets(
  args: {
    document_id: string;
    match_prefix?: string;
    bullet_preset?: string;
    tab_id?: string;
    dry_run?: boolean;
  },
  ctx: Ctx,
): Promise<BulletsResult> {
  const matchPrefix = args.match_prefix ?? "- ";
  const bulletPreset = args.bullet_preset ?? DEFAULT_BULLET_PRESET;
  if (!matchPrefix) throw new Error("match_prefix must not be empty");
  checkBulletPreset(bulletPreset);

  const { document } = await get_document({ document_id: args.document_id }, ctx);
  const { matches, alreadyBulleted } = buildBulletMatches(document, matchPrefix, args.tab_id);
  const summary: BulletsResult = {
    document_id: documentId(args.document_id),
    match_prefix: matchPrefix,
    bullet_preset: bulletPreset,
    matched_paragraphs: matches.length,
    updated_paragraphs: 0,
    verified_paragraphs: 0,
    already_bulleted_paragraphs: alreadyBulleted,
    dry_run: args.dry_run === true,
    paragraphs: matches.map((match) => ({
      tab_id: match.tab_id,
      paragraph_index: match.paragraph_index,
      before: match.before,
      after: match.after,
      nesting_level: match.nesting_level,
    })),
  };
  if (args.dry_run || matches.length === 0) return summary;

  const sorted = [...matches].sort((left, right) => {
    const tabOrder = (left.tab_id ?? "").localeCompare(right.tab_id ?? "");
    if (tabOrder !== 0) return -tabOrder;
    return right.start_index - left.start_index;
  });
  const requests: unknown[] = [];
  for (const match of sorted) {
    const deleteRange: Record<string, unknown> = {
      startIndex: match.delete_start,
      endIndex: match.delete_end,
    };
    const bulletRange: Record<string, unknown> = {
      startIndex: match.start_index,
      endIndex: match.end_index,
    };
    if (match.tab_id) {
      deleteRange.tabId = match.tab_id;
      bulletRange.tabId = match.tab_id;
    }
    requests.push({ deleteContentRange: { range: deleteRange } });
    requests.push({ createParagraphBullets: { range: bulletRange, bulletPreset } });
  }

  const revisionId = document.revisionId;
  if (!revisionId) {
    throw new Error("Google Docs response did not include revisionId for the guarded update");
  }
  await batch_update(
    { document_id: summary.document_id, requests, required_revision_id: revisionId },
    ctx,
  );

  const { document: verified } = await get_document({ document_id: summary.document_id }, ctx);
  const byKey: Record<string, Paragraph> = {};
  for (const paragraph of documentParagraphs(verified)) {
    byKey[`${paragraph.tab_id ?? ""}\u0000${paragraph.paragraph_index}`] = paragraph;
  }
  let verifiedCount = 0;
  for (const match of matches) {
    const paragraph = byKey[`${match.tab_id ?? ""}\u0000${match.paragraph_index}`];
    if (!paragraph) continue;
    if (paragraph.has_bullet && paragraph.text.replace(/\n+$/, "") === match.after) verifiedCount += 1;
  }
  summary.updated_paragraphs = matches.length;
  summary.verified_paragraphs = verifiedCount;
  return summary;
}
