import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Console-authored skills for the sandbox's principal, through the sandbox's iron-proxy, which
// injects the short-lived sandbox entitlement. Results are Console's own payloads with the
// `{ data: ... }` envelope removed. Repository skills (`.agents/skills` on disk, `repo:` ids) are
// read from the filesystem by the Python tool and are not reachable from a connector.

const DEFAULT_BASE = "http://centaur-console:3000";
const SKILLS_PATH = "/api/v1/sandbox/skills";

/** Console clamps list and search limits to this range. */
const MAX_LIMIT = 20;

/** A Console skill catalog entry. */
export interface Skill {
  id: string;
  /** Unique skill name, such as `incident-triage`. */
  name: string;
  /** When to use the skill, as shown in the catalog. */
  description: string;
  /** `private`: only the author and editors see it. `shared`: every Console user sees it. */
  visibility: "private" | "shared";
  /** Display name or email of the author. */
  author: string;
  updated_at: string | null;
  checksum: string;
  /** The complete SKILL.md. `read`, `create` and `edit` include it; list and search do not. */
  document?: string;
  /** Optimistic-concurrency version; `create` and `edit` return it, `edit` accepts it. */
  lock_version?: number;
  [field: string]: unknown;
}

/** A Console user who may edit a skill. */
export interface SkillEditor {
  id: string;
  email: string;
  name?: string | null;
  status?: string;
  [field: string]: unknown;
}

/** The editing state of one skill. */
export interface EditorList {
  id: string;
  editors: SkillEditor[];
  /** Pass this to `edit` as `lock_version` to reject a concurrent change. */
  lock_version: number;
}

/** Non-secret endpoint configuration; sandboxes receive `CENTAUR_CONSOLE_URL` from api-rs. */
function baseUrl(ctx: Ctx): string {
  const raw = (ctx.env.CENTAUR_CONSOLE_URL || DEFAULT_BASE).trim().replace(/\/+$/, "");
  if (raw && !raw.startsWith("http://") && !raw.startsWith("https://")) return `http://${raw}`;
  return raw;
}

function headers(ctx: Ctx, hasBody: boolean): Record<string, string> {
  const result: Record<string, string> = { accept: "application/json" };
  // iron-proxy injects the sandbox entitlement on the wire; this is a local/debug override.
  const bearer = (ctx.env.CENTAUR_CONSOLE_BEARER_TOKEN || "").trim();
  if (bearer) result.authorization = `Bearer ${bearer}`;
  if (hasBody) result["content-type"] = "application/json";
  return result;
}

/** One Console request; returns the parsed body, or undefined for an empty 204 response. */
async function request(
  ctx: Ctx,
  path: string,
  options: { method?: string; params?: Params; data?: unknown } = {},
): Promise<any> {
  const res = await ctx.fetch(baseUrl(ctx) + path + query(options.params ?? {}), {
    method: options.method ?? "GET",
    headers: headers(ctx, options.data !== undefined),
    body: options.data === undefined ? undefined : JSON.stringify(options.data),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`centaur-skills returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return text ? JSON.parse(text) : undefined;
}

function dataObject(payload: any): any {
  const data = payload && typeof payload === "object" ? payload.data : undefined;
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    throw new Error("centaur-skills response did not include a data object");
  }
  return data;
}

function dataArray(payload: any): any[] {
  const data = payload && typeof payload === "object" ? payload.data : undefined;
  if (!Array.isArray(data)) throw new Error("centaur-skills response did not include a data array");
  return data;
}

function skillPath(identifier: string): string {
  return `${SKILLS_PATH}/${encodeURIComponent(identifier)}`;
}

/**
 * List Console skills visible to the caller, most recently updated first. `scope` narrows to the
 * caller's `private` skills or to `shared` skills visible to everyone; omit it for both. `limit`
 * defaults to 20 and is clamped to 20.
 */
export async function list(
  args: { scope?: "private" | "shared"; limit?: number },
  ctx: Ctx,
): Promise<{ skills: Skill[] }> {
  const params: Params = { limit: args.limit === undefined ? MAX_LIMIT : args.limit };
  if (args.scope) params.scope = args.scope;
  return { skills: dataArray(await request(ctx, SKILLS_PATH, { params })) };
}

/**
 * Full-text search over Console skills visible to the caller, best match first. `query` is the task
 * or capability to find guidance for. `limit` defaults to 10 and is clamped to 20.
 */
export async function search(
  args: { query: string; limit?: number },
  ctx: Ctx,
): Promise<{ skills: Skill[] }> {
  const payload = await request(ctx, `${SKILLS_PATH}/search`, {
    params: { q: args.query, limit: args.limit === undefined ? 10 : args.limit },
  });
  return { skills: dataArray(payload) };
}

/**
 * Read one visible Console skill by exact name or OID (`skl_...`) and return its complete SKILL.md
 * in `document`, plus its metadata and checksum.
 */
export async function read(args: { identifier: string }, ctx: Ctx): Promise<{ skill: Skill }> {
  return { skill: dataObject(await request(ctx, skillPath(args.identifier))) };
}

/**
 * Create a Console skill owned by the caller. `name` must be a unique lowercase skill name,
 * `description` says when to use it, and `instructions` is the Markdown body of SKILL.md.
 * `visibility` defaults to `shared`. Returns the skill with its document and `lock_version`.
 */
export async function create(
  args: {
    name: string;
    description: string;
    instructions: string;
    visibility?: "private" | "shared";
  },
  ctx: Ctx,
): Promise<{ skill: Skill }> {
  const data = {
    name: args.name,
    description: args.description,
    instructions: args.instructions,
    visibility: args.visibility === undefined ? "shared" : args.visibility,
  };
  return { skill: dataObject(await request(ctx, SKILLS_PATH, { method: "POST", data: { data } })) };
}

/**
 * Edit fields of a skill the caller owns or may edit, by OID (`skl_...`); omitted fields are kept.
 * Pass `lock_version` to fail instead of overwriting a skill that changed since it was read.
 * Visibility is not editable here: use `share` or `unshare`.
 */
export async function edit(
  args: {
    identifier: string;
    name?: string;
    description?: string;
    instructions?: string;
    lock_version?: number;
  },
  ctx: Ctx,
): Promise<{ skill: Skill }> {
  const data: Record<string, unknown> = {};
  for (const key of ["name", "description", "instructions"] as const) {
    const value = args[key];
    if (value !== undefined) data[key] = value;
  }
  if (Object.keys(data).length === 0) {
    throw new Error("at least one skill field must be provided");
  }
  if (args.lock_version !== undefined) data.lock_version = args.lock_version;
  return {
    skill: dataObject(
      await request(ctx, skillPath(args.identifier), { method: "PATCH", data: { data } }),
    ),
  };
}

/** Archive a skill the caller owns, by OID (`skl_...`). Archived skills leave every catalog. */
export async function delete_skill(
  args: { identifier: string },
  ctx: Ctx,
): Promise<{ id: string; archived: boolean }> {
  await request(ctx, skillPath(args.identifier), { method: "DELETE" });
  return { id: args.identifier, archived: true };
}

/** Share a skill the caller owns, by OID: every Console user can then discover and read it. */
export async function share(args: { identifier: string }, ctx: Ctx): Promise<{ skill: Skill }> {
  return {
    skill: dataObject(await request(ctx, `${skillPath(args.identifier)}/share`, { method: "POST" })),
  };
}

/** Make a skill the caller owns private again, by OID: only its author and editors keep access. */
export async function unshare(args: { identifier: string }, ctx: Ctx): Promise<{ skill: Skill }> {
  return {
    skill: dataObject(
      await request(ctx, `${skillPath(args.identifier)}/unshare`, { method: "POST" }),
    ),
  };
}

/** List the editors of one visible skill by exact name or OID, with its current `lock_version`. */
export async function list_editors(
  args: { identifier: string },
  ctx: Ctx,
): Promise<EditorList> {
  return dataObject(await request(ctx, `${skillPath(args.identifier)}/editors`));
}

/**
 * Add an editor to a skill the caller owns: an exact Console user email or a `usr_...` OID. Returns
 * the skill's editor list.
 */
export async function add_editor(
  args: { identifier: string; user: string },
  ctx: Ctx,
): Promise<EditorList> {
  const user = args.user.trim();
  if (!user) throw new Error("user email or OID must not be empty");
  return dataObject(
    await request(ctx, `${skillPath(args.identifier)}/editors`, {
      method: "POST",
      data: { data: { user } },
    }),
  );
}

/**
 * Remove an editor from a skill the caller owns: an exact Console user email or a `usr_...` OID.
 * Returns the skill's editor list.
 */
export async function remove_editor(
  args: { identifier: string; user: string },
  ctx: Ctx,
): Promise<EditorList> {
  const user = args.user.trim();
  if (!user) throw new Error("user email or OID must not be empty");
  return dataObject(
    await request(ctx, `${skillPath(args.identifier)}/editors`, {
      method: "DELETE",
      data: { data: { user } },
    }),
  );
}
