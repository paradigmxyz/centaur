import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are Notion's own objects, passed through; nothing is renamed or
// reformatted. Field names and value shapes are Notion's
// (https://developers.notion.com/reference). List endpoints return Notion's
// `{ object: "list", results, next_cursor, has_more }` envelope.

const API_BASE = "https://api.notion.com/v1";
// The version the Python client pins. Notion's 2025 data-source API is not used here.
const NOTION_VERSION = "2022-06-28";

/** A rich text span. `plain_text` is the rendered text; `text.content` the raw content. */
export interface RichText {
  type: string;
  plain_text: string;
  href?: string | null;
  text?: { content: string; link?: { url: string } | null; [field: string]: unknown };
  annotations?: {
    bold?: boolean;
    italic?: boolean;
    strikethrough?: boolean;
    underline?: boolean;
    code?: boolean;
    color?: string;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** Notion's list envelope; `next_cursor` is passed back as `start_cursor` while `has_more`. */
export interface ListResponse<T> {
  object: "list";
  results: T[];
  next_cursor: string | null;
  has_more: boolean;
  type?: string;
  [field: string]: unknown;
}

/** A workspace user. `type` is person or bot; people carry `person.email`. */
export interface NotionUser {
  object: "user";
  id: string;
  type?: string;
  name?: string;
  avatar_url?: string | null;
  person?: { email?: string; [field: string]: unknown };
  bot?: { owner?: unknown; workspace_name?: string | null; [field: string]: unknown };
  [field: string]: unknown;
}

/** A page. `properties` keys are property names; each value is Notion's page property value object. */
export interface NotionPage {
  object: "page";
  id: string;
  created_time?: string;
  last_edited_time?: string;
  archived?: boolean;
  url?: string;
  icon?: unknown;
  cover?: unknown;
  parent?: { type?: string; page_id?: string; database_id?: string; workspace?: boolean; [field: string]: unknown };
  properties: { [property: string]: unknown };
  [field: string]: unknown;
}

/** A database. `properties` is the schema: property name to `{ id, name, type, ... }`. */
export interface NotionDatabase {
  object: "database";
  id: string;
  title: RichText[];
  description?: RichText[];
  properties: { [property: string]: unknown };
  parent?: { type?: string; page_id?: string; workspace?: boolean; [field: string]: unknown };
  url?: string;
  is_inline?: boolean;
  archived?: boolean;
  created_time?: string;
  last_edited_time?: string;
  [field: string]: unknown;
}

/** A block. The payload lives under the block's own type key, e.g. `block.paragraph.rich_text`. */
export interface NotionBlock {
  object: "block";
  id: string;
  type: string;
  has_children?: boolean;
  archived?: boolean;
  created_time?: string;
  last_edited_time?: string;
  [field: string]: unknown;
}

/** A comment. `rich_text` holds the body. */
export interface NotionComment {
  object: "comment";
  id: string;
  discussion_id?: string;
  created_time?: string;
  last_edited_time?: string;
  parent?: { [field: string]: unknown };
  rich_text: RichText[];
  [field: string]: unknown;
}

/** A page property item, as `page_property` returns it: `{ type, <type>: value }`, sometimes with pagination. */
export interface NotionPropertyItem {
  object?: string;
  id?: string;
  type?: string;
  has_more?: boolean;
  next_cursor?: string | null;
  [field: string]: unknown;
}

type Body = { [field: string]: unknown };

/**
 * A Notion ID from an ID or URL: URL slugs (`.../My-Page-8a4f...`) and dashed UUIDs are normalized.
 * Returns the ID Notion's API accepts, or the trimmed input when it already looks like one.
 */
function notionId(value: string): string {
  const raw = value.trim();
  if (raw.includes("notion.so") || raw.includes("notion.site") || raw.includes("notion.com")) {
    const path = raw.split("?")[0].split("#")[0].replace(/\/+$/, "");
    const last = path.split("/").pop() ?? raw;
    const match = /([0-9a-f]{32})$/i.exec(last.replace(/-/g, ""));
    return match ? match[1] : last;
  }
  return raw.replace(/-/g, "");
}

/** Normalize the parent IDs a page/database body may carry. */
function parentIds(parent: Body): Body {
  const out: Body = { ...parent };
  for (const key of ["page_id", "database_id", "block_id"]) {
    const value = out[key];
    if (typeof value === "string") out[key] = notionId(value);
  }
  return out;
}

async function request(
  ctx: Ctx,
  label: string,
  method: string,
  path: string,
  options: { body?: unknown; params?: Params } = {},
): Promise<any> {
  const headers: Record<string, string> = {
    accept: "application/json",
    "notion-version": NOTION_VERSION,
  };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.NOTION_API_KEY) headers.authorization = `Bearer ${ctx.env.NOTION_API_KEY}`;
  if (options.body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(`${API_BASE}${path}${query(options.params ?? {})}`, {
    method,
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  const text = await res.text();
  if (!res.ok) {
    // Notion errors are `{ object: "error", status, code, message }`; the message names the
    // offending property or value.
    let code: string | null = null;
    let message: string | null = null;
    try {
      const body = JSON.parse(text);
      code = typeof body?.code === "string" ? body.code : null;
      message = typeof body?.message === "string" ? body.message : null;
    } catch {
      message = null;
    }
    const detail = message ?? (text ? text.slice(0, 300) : "no response body");
    throw new Error(`Notion ${label} failed (HTTP ${res.status}${code ? ` ${code}` : ""}): ${detail}`);
  }
  return text ? JSON.parse(text) : null;
}

const pageSize = (value: number | undefined, fallback: number): number => Math.max(1, Math.min(value ?? fallback, 100));

/** Follow `next_cursor` up to `max_pages` pages and concatenate `results`. */
async function paginate(
  page: (cursor: string | undefined) => Promise<ListResponse<any>>,
  maxPages: number,
): Promise<{ items: any[]; has_more: boolean }> {
  const items: any[] = [];
  let cursor: string | undefined;
  for (let index = 0; index < maxPages; index += 1) {
    const reply = await page(cursor);
    for (const item of reply?.results ?? []) items.push(item);
    cursor = reply?.next_cursor ?? undefined;
    if (!reply?.has_more || !cursor) return { items, has_more: false };
  }
  return { items, has_more: true };
}

const richText = (text: string): Body[] => [{ type: "text", text: { content: text } }];

/**
 * Search pages and databases by title, newest-edited first by default. `query` matches titles (omit
 * it to list everything the integration can see); `filter_type` restricts to page or database;
 * `page_size` is 1-100 (default 100). Follow `next_cursor` as `start_cursor` while `has_more`.
 */
export async function search(
  args: {
    query?: string;
    filter_type?: "page" | "database";
    sort_direction?: "ascending" | "descending";
    sort_timestamp?: "last_edited_time";
    page_size?: number;
    start_cursor?: string;
  },
  ctx: Ctx,
): Promise<ListResponse<NotionPage | NotionDatabase>> {
  const body: Body = { page_size: pageSize(args.page_size, 100) };
  if (args.query) body.query = args.query;
  if (args.filter_type) body.filter = { property: "object", value: args.filter_type };
  body.sort = {
    direction: args.sort_direction ?? "descending",
    timestamp: args.sort_timestamp ?? "last_edited_time",
  };
  if (args.start_cursor) body.start_cursor = args.start_cursor;
  return request(ctx, "search", "POST", "/search", { body });
}

/** The integration's own bot user: its ID, name and workspace. The cheapest auth check. */
export async function me(args: {}, ctx: Ctx): Promise<{ user: NotionUser }> {
  return { user: await request(ctx, "users.me", "GET", "/users/me") };
}

/**
 * Workspace users, with type (person or bot), name and email for people. `page_size` is 1-100
 * (default 100); follow `next_cursor` as `start_cursor`.
 */
export async function users(
  args: { page_size?: number; start_cursor?: string },
  ctx: Ctx,
): Promise<ListResponse<NotionUser>> {
  return request(ctx, "users", "GET", "/users", {
    params: { page_size: pageSize(args.page_size, 100), start_cursor: args.start_cursor },
  });
}

/** One user by ID (`user_id` accepts an ID or a URL). */
export async function user(args: { user_id: string }, ctx: Ctx): Promise<{ user: NotionUser }> {
  return { user: await request(ctx, "users.retrieve", "GET", `/users/${encodeURIComponent(notionId(args.user_id))}`) };
}

/** One database by ID or URL: title, schema (`properties` name to type and options), parent, URL. */
export async function database(args: { database_id: string }, ctx: Ctx): Promise<{ database: NotionDatabase }> {
  return { database: await request(ctx, "databases.retrieve", "GET", `/databases/${encodeURIComponent(notionId(args.database_id))}`) };
}

/**
 * One page of database rows. `filter` and `sorts` are Notion's own objects, passed through, e.g.
 * `{ property: "Status", status: { equals: "Done" } }` and
 * `[{ property: "Created", direction: "descending" }]`; combine conditions with
 * `{ and: [...] }` / `{ or: [...] }`. `page_size` is 1-100 (default 100); follow `next_cursor`
 * as `start_cursor`. Rows are pages: read `properties` for values.
 */
export async function query_database(
  args: {
    database_id: string;
    filter?: Body;
    sorts?: Body[];
    page_size?: number;
    start_cursor?: string;
  },
  ctx: Ctx,
): Promise<ListResponse<NotionPage>> {
  const body: Body = { page_size: pageSize(args.page_size, 100) };
  if (args.filter) body.filter = args.filter;
  if (args.sorts?.length) body.sorts = args.sorts;
  if (args.start_cursor) body.start_cursor = args.start_cursor;
  return request(ctx, "databases.query", "POST", `/databases/${encodeURIComponent(notionId(args.database_id))}/query`, {
    body,
  });
}

/**
 * Every page of a database query, following `next_cursor` for up to `max_pages` pages of
 * `page_size` (1-100, default 100) rows each. `has_more` is true when the call stopped at
 * `max_pages` with pages left, so narrow the filter or raise `max_pages`.
 */
export async function database_pages(
  args: {
    database_id: string;
    filter?: Body;
    sorts?: Body[];
    page_size?: number;
    max_pages?: number;
  },
  ctx: Ctx,
): Promise<{ database_id: string; count: number; pages: NotionPage[]; has_more: boolean }> {
  const database_id = notionId(args.database_id);
  const size = pageSize(args.page_size, 100);
  const { items, has_more } = await paginate(
    (cursor) => {
      const body: Body = { page_size: size };
      if (args.filter) body.filter = args.filter;
      if (args.sorts?.length) body.sorts = args.sorts;
      if (cursor) body.start_cursor = cursor;
      return request(ctx, "databases.query", "POST", `/databases/${encodeURIComponent(database_id)}/query`, { body });
    },
    Math.max(1, args.max_pages ?? 20),
  );
  return { database_id, count: items.length, pages: items, has_more };
}

/**
 * Create a database inside a page. `properties` is the schema, keyed by property name, with the
 * title property required, e.g. `{ Name: { title: {} }, Status: { select: { options: [{ name: "Todo" }] } } }`.
 * `is_inline` creates it embedded in the page rather than as a child page.
 */
export async function create_database(
  args: {
    parent_page_id: string;
    title: string;
    properties: Body;
    is_inline?: boolean;
  },
  ctx: Ctx,
): Promise<{ database: NotionDatabase }> {
  const body: Body = {
    parent: { type: "page_id", page_id: notionId(args.parent_page_id) },
    title: richText(args.title),
    properties: args.properties,
    is_inline: args.is_inline ?? false,
  };
  return { database: await request(ctx, "databases.create", "POST", "/databases", { body }) };
}

/**
 * Update a database's title and/or schema. `properties` may add new properties, or rename/reconfigure
 * existing ones; a property whose name is an empty string is removed. Omitted fields are unchanged.
 */
export async function update_database(
  args: { database_id: string; title?: string; properties?: Body },
  ctx: Ctx,
): Promise<{ database: NotionDatabase }> {
  const body: Body = {};
  if (args.title) body.title = richText(args.title);
  if (args.properties) body.properties = args.properties;
  return {
    database: await request(ctx, "databases.update", "PATCH", `/databases/${encodeURIComponent(notionId(args.database_id))}`, {
      body,
    }),
  };
}

/**
 * One page by ID or URL: properties (typed values), parent, icon, cover, URL and timestamps. Read the
 * body separately with `page_content` or a single level with `block_children`.
 */
export async function page(args: { page_id: string }, ctx: Ctx): Promise<{ page: NotionPage }> {
  return { page: await request(ctx, "pages.retrieve", "GET", `/pages/${encodeURIComponent(notionId(args.page_id))}`) };
}

/**
 * One page property's value, page by page for properties that paginate (title, rich_text, relation).
 * `property_id` is a property ID or name; follow `next_cursor` as `start_cursor` while `has_more`.
 */
export async function page_property(
  args: { page_id: string; property_id: string; page_size?: number; start_cursor?: string },
  ctx: Ctx,
): Promise<{ property: NotionPropertyItem }> {
  const property = await request(
    ctx,
    "pages.properties.retrieve",
    "GET",
    `/pages/${encodeURIComponent(notionId(args.page_id))}/properties/${encodeURIComponent(args.property_id)}`,
    { params: { page_size: pageSize(args.page_size, 100), start_cursor: args.start_cursor } },
  );
  return { property };
}

/**
 * Create a page. `parent` is `{ database_id }` for a database row or `{ page_id }` for a child page.
 * `properties` uses Notion's page property values; for a database row it must include the database's
 * title property by name, e.g. `{ Name: { title: [{ text: { content: "Task" } }] } }`; a child page
 * takes `{ title: { title: [{ text: { content: "Title" } }] } }`. `children` are block objects for
 * the initial body; `icon`/`cover` are Notion's icon and cover objects.
 */
export async function create_page(
  args: {
    parent: Body;
    properties: Body;
    children?: Body[];
    icon?: Body;
    cover?: Body;
  },
  ctx: Ctx,
): Promise<{ page: NotionPage }> {
  const body: Body = { parent: parentIds(args.parent), properties: args.properties };
  if (args.children?.length) body.children = args.children;
  if (args.icon) body.icon = args.icon;
  if (args.cover) body.cover = args.cover;
  return { page: await request(ctx, "pages.create", "POST", "/pages", { body }) };
}

/**
 * Set a page's properties, e.g. the fields of a database row. `properties` is keyed by property name
 * and uses Notion's page property values, e.g.
 * `{ Priority: { select: { name: "P0" } }, Due: { date: { start: "2026-01-31" } } }`; omitted
 * properties are unchanged. Clear a value with null (select, status, date, number, url, checkbox's
 * false) or [] (multi_select, people, relation, rich_text). `archived` trashes or restores the page;
 * `icon`/`cover` replace them.
 */
export async function update_page(
  args: {
    page_id: string;
    properties?: Body;
    archived?: boolean;
    icon?: Body;
    cover?: Body;
  },
  ctx: Ctx,
): Promise<{ page: NotionPage }> {
  const body: Body = {};
  if (args.properties) body.properties = args.properties;
  if (args.archived !== undefined) body.archived = args.archived;
  if (args.icon) body.icon = args.icon;
  if (args.cover) body.cover = args.cover;
  return {
    page: await request(ctx, "pages.update", "PATCH", `/pages/${encodeURIComponent(notionId(args.page_id))}`, { body }),
  };
}

/** Move a page to the trash (sets `archived` true). */
export async function archive_page(args: { page_id: string }, ctx: Ctx): Promise<{ page: NotionPage }> {
  return {
    page: await request(ctx, "pages.update", "PATCH", `/pages/${encodeURIComponent(notionId(args.page_id))}`, {
      body: { archived: true },
    }),
  };
}

/** Restore a page from the trash (sets `archived` false). */
export async function restore_page(args: { page_id: string }, ctx: Ctx): Promise<{ page: NotionPage }> {
  return {
    page: await request(ctx, "pages.update", "PATCH", `/pages/${encodeURIComponent(notionId(args.page_id))}`, {
      body: { archived: false },
    }),
  };
}

/** One block by ID: its type, the payload under its type key, `has_children` and timestamps. */
export async function block(args: { block_id: string }, ctx: Ctx): Promise<{ block: NotionBlock }> {
  return { block: await request(ctx, "blocks.retrieve", "GET", `/blocks/${encodeURIComponent(notionId(args.block_id))}`) };
}

/**
 * One page of a page's or block's direct children, in document order. `page_size` is 1-100 (default
 * 100); follow `next_cursor` as `start_cursor` while `has_more`. Blocks with `has_children` expand
 * with another `block_children` call on their ID.
 */
export async function block_children(
  args: { block_id: string; page_size?: number; start_cursor?: string },
  ctx: Ctx,
): Promise<ListResponse<NotionBlock>> {
  return request(ctx, "blocks.children.list", "GET", `/blocks/${encodeURIComponent(notionId(args.block_id))}/children`, {
    params: { page_size: pageSize(args.page_size, 100), start_cursor: args.start_cursor },
  });
}

/**
 * All direct children of a page or block, following `next_cursor` for up to `max_pages` pages of
 * `page_size` (1-100, default 100) blocks each. Returns the flat top-level list: nested children of
 * blocks with `has_children` are not expanded. `has_more` is true when the call stopped at
 * `max_pages` with pages left.
 */
export async function page_content(
  args: { block_id: string; page_size?: number; max_pages?: number },
  ctx: Ctx,
): Promise<{ block_id: string; count: number; blocks: NotionBlock[]; has_more: boolean }> {
  const block_id = notionId(args.block_id);
  const size = pageSize(args.page_size, 100);
  const { items, has_more } = await paginate(
    (cursor) =>
      request(ctx, "blocks.children.list", "GET", `/blocks/${encodeURIComponent(block_id)}/children`, {
        params: { page_size: size, start_cursor: cursor },
      }),
    Math.max(1, args.max_pages ?? 20),
  );
  return { block_id, count: items.length, blocks: items, has_more };
}

/**
 * Write: append block objects to a page or block (Notion appends at most 100 children per request).
 * Each block is `{ object: "block", type, [type]: { ... } }`, e.g.
 * `{ type: "paragraph", paragraph: { rich_text: [{ type: "text", text: { content: "Hi" } }] } }`.
 * `after` inserts after that block ID instead of at the end. For plain text use `append_text`.
 */
export async function append_block_children(
  args: { block_id: string; children: Body[]; after?: string },
  ctx: Ctx,
): Promise<ListResponse<NotionBlock>> {
  if (args.children.length > 100) throw new Error("Notion accepts at most 100 children per append; split the list.");
  const body: Body = { children: args.children };
  if (args.after) body.after = notionId(args.after);
  return request(ctx, "blocks.children.append", "PATCH", `/blocks/${encodeURIComponent(notionId(args.block_id))}/children`, {
    body,
  });
}

const BLOCK_TYPES: { [alias: string]: string } = {
  paragraph: "paragraph",
  heading_1: "heading_1",
  heading_2: "heading_2",
  heading_3: "heading_3",
  heading1: "heading_1",
  heading2: "heading_2",
  heading3: "heading_3",
  bullet: "bulleted_list_item",
  bulleted_list_item: "bulleted_list_item",
  numbered_list_item: "numbered_list_item",
  todo: "to_do",
  to_do: "to_do",
  toggle: "toggle",
  quote: "quote",
  code: "code",
};

/**
 * Write: append one plain-text block to a page or block. `block_type` is paragraph (default),
 * heading_1..3 (aliases heading1..3), bullet (bulleted_list_item), numbered_list_item, to_do (alias
 * todo, with `checked`), toggle, quote or code (with `language`, default "plain text"). Use
 * `append_block_children` for rich text, mentions or other block payloads.
 */
export async function append_text(
  args: {
    block_id: string;
    text: string;
    block_type?: string;
    checked?: boolean;
    language?: string;
  },
  ctx: Ctx,
): Promise<ListResponse<NotionBlock>> {
  const block_type = BLOCK_TYPES[args.block_type ?? "paragraph"];
  if (!block_type) throw new Error(`unknown block_type ${JSON.stringify(args.block_type)}; see append_text's description`);
  const payload: Body = { rich_text: richText(args.text) };
  if (block_type === "to_do") payload.checked = args.checked ?? false;
  if (block_type === "code") payload.language = args.language ?? "plain text";
  const body: Body = { children: [{ object: "block", type: block_type, [block_type]: payload }] };
  return request(ctx, "blocks.children.append", "PATCH", `/blocks/${encodeURIComponent(notionId(args.block_id))}/children`, {
    body,
  });
}

/**
 * Update a block's payload. `block` carries only the type key to change, e.g.
 * `{ paragraph: { rich_text: [{ type: "text", text: { content: "New text" } }] } }`; `archived`
 * true deletes it (see `delete_block`). Types differ in what they accept; for example a to_do also
 * takes `checked`.
 */
export async function update_block(
  args: { block_id: string; block: Body; archived?: boolean },
  ctx: Ctx,
): Promise<{ block: NotionBlock }> {
  const body: Body = { ...args.block };
  if (args.archived !== undefined) body.archived = args.archived;
  return {
    block: await request(ctx, "blocks.update", "PATCH", `/blocks/${encodeURIComponent(notionId(args.block_id))}`, { body }),
  };
}

/** Write: delete a block by moving it to the trash (Notion returns the block with `archived` true). */
export async function delete_block(args: { block_id: string }, ctx: Ctx): Promise<{ block: NotionBlock }> {
  return { block: await request(ctx, "blocks.delete", "DELETE", `/blocks/${encodeURIComponent(notionId(args.block_id))}`) };
}

/**
 * One page of comments on a page or block, oldest first. `block_id` accepts a page ID or a block ID;
 * omit it to list all comments the integration can see. `page_size` is 1-100 (default 100); follow
 * `next_cursor` as `start_cursor` while `has_more`.
 */
export async function comments(
  args: { block_id?: string; page_size?: number; start_cursor?: string },
  ctx: Ctx,
): Promise<ListResponse<NotionComment>> {
  return request(ctx, "comments.list", "GET", "/comments", {
    params: {
      block_id: args.block_id === undefined ? undefined : notionId(args.block_id),
      page_size: pageSize(args.page_size, 100),
      start_cursor: args.start_cursor,
    },
  });
}

/**
 * Write: comment on a page (`parent` `{ page_id }`) or reply in a discussion (`{ discussion_id }`,
 * from an existing comment). `text` becomes one plain-text rich text span; use `rich_text` for
 * mentions, links or formatting. An integration can only comment on pages it can access.
 */
export async function create_comment(
  args: { parent: Body; text?: string; rich_text?: Body[] },
  ctx: Ctx,
): Promise<{ comment: NotionComment }> {
  const content = args.rich_text ?? (args.text === undefined ? undefined : richText(args.text));
  if (!content?.length) throw new Error("create_comment needs text or rich_text");
  const parent = parentIds(args.parent);
  const comment = await request(ctx, "comments.create", "POST", "/comments", {
    body: { parent, rich_text: content },
  });
  return { comment };
}

/**
 * Plain-text title of a page or database ("" when it has none): the `title` property for a page, the
 * `title` array for a database. No network call; handy for labeling search results.
 */
export async function extract_title(
  args: { page_or_database: NotionPage | NotionDatabase },
  ctx: Ctx,
): Promise<{ title: string }> {
  const object = args.page_or_database;
  if (!object || typeof object !== "object") return { title: "" };
  if (object.object === "database") {
    return { title: (object.title ?? []).map((item) => item.plain_text ?? "").join("") };
  }
  const properties = object.properties ?? {};
  for (const key of Object.keys(properties)) {
    const property = properties[key] as { type?: string; title?: RichText[] } | null;
    if (property?.type === "title") {
      return { title: (property.title ?? []).map((item) => item.plain_text ?? "").join("") };
    }
  }
  return { title: "" };
}
