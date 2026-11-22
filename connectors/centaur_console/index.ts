import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Centaur Console API, reached through the sandbox's iron-proxy, which injects the short-lived
// sandbox entitlement for the caller's principal. Results are Console's own payloads with the
// `{ data: ... }` envelope removed. Admin calls use the console API as the linked Console admin;
// they answer only in an active admin's Slack DM.

const DEFAULT_BASE = "http://centaur-console:3000";
const PERMISSIONS_PATH = "/api/v1/sandbox/permissions";
const OAUTH_APPS_PATH = "/api/v1/sandbox/oauth_apps";
const SCHEDULED_TASKS_PATH = "/api/v1/sandbox/scheduled_tasks";
const ADMIN_PRINCIPALS_PATH = "/api/v1/console/principals";
const ADMIN_ROLES_PATH = "/api/v1/console/roles";
const ADMIN_GRANTS_PATH = "/api/v1/console/grants";
const ADMIN_SECRETS_PATH = "/api/v1/console/secrets";
const ADMIN_SLACK_EXTERNAL_TEAMS_PATH = "/api/v1/console/system_settings/slack_external_teams";
const ADMIN_PAGE_LIMIT = 200;

// Secret kinds an admin can create and edit from a sandbox, by grant key prefix.
const ADMIN_WRITABLE_SECRET_PATHS: Record<string, string> = {
  static: "/api/v1/console/static_secrets",
  pg_dsn: "/api/v1/console/pg_dsn_secrets",
  gcp_auth: "/api/v1/console/gcp_auth_secrets",
  gcp_id_token: "/api/v1/console/gcp_id_token_secrets",
};

/** A Slack channel permission row. New rows enable every flag that was left unspecified. */
export interface SlackChannelPermission {
  channel_id: string;
  upload_enabled: boolean;
  download_enabled: boolean;
  history_enabled: boolean;
  [field: string]: unknown;
}

/** An OAuth app the sandbox can start a consent flow for. */
export interface OAuthApp {
  id: string;
  slug: string;
  description?: string | null;
  labels?: Record<string, unknown>;
  provider: string;
  allowed_scopes?: string[];
  /** Console URL that starts the consent flow; open it in a browser. */
  start_url: string;
  [field: string]: unknown;
}

/** The OAuth credential the principal already holds for a provider. */
export interface OAuthCredential {
  id: string;
  oauth_app_id: string;
  slug: string;
  provider: string;
  provider_email?: string | null;
  provider_subject?: string | null;
  /** `live` when usable. */
  status: string;
  scopes?: string[];
  [field: string]: unknown;
}

/** A scheduled task owned by the calling user, or by the channel in a channel thread. */
export interface ScheduledTask {
  id: string;
  name: string;
  prompt: string;
  /** Slack channel or user id that receives the run's output. */
  delivery_channel: string;
  /** Five-field cron expression in the task's timezone; null for a manual task. */
  cron_expression: string | null;
  timezone: string;
  schedule_label: string;
  enabled: boolean;
  next_run_at?: string | null;
  last_enqueued_at?: string | null;
  last_run_id?: string | null;
  last_run_at?: string | null;
  last_error?: string | null;
  created_at?: string;
  updated_at?: string;
  /** Set by `run_scheduled_task`: Console queued an immediate run. */
  queued?: boolean;
  [field: string]: unknown;
}

/** A principal record, as the admin API sends it. */
export interface Principal {
  id: string;
  foreign_id: string;
  name: string;
  labels?: Record<string, unknown>;
  slack_channel_permissions?: SlackChannelPermission[];
  effective_slack_channel_permissions?: SlackChannelPermission[];
  sandbox_repo_cache?: string;
  sandbox_observability_enabled?: boolean;
  sandbox_sessions_read_enabled?: boolean;
  sandbox_workflows_read_enabled?: boolean;
  sandbox_workflows_write_enabled?: boolean;
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

/** A role that carries secret grants and Slack channel permissions. */
export interface Role {
  id: string;
  foreign_id: string;
  name: string;
  labels?: Record<string, unknown>;
  slack_channel_permissions?: SlackChannelPermission[];
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

/** A direct secret grant to a principal or a role. */
export interface Grant {
  id: string;
  principal_id?: string;
  role_id?: string;
  static_secret_id?: string;
  gcp_auth_secret_id?: string;
  gcp_id_token_secret_id?: string;
  aws_auth_secret_id?: string;
  oauth_token_secret_id?: string;
  pg_dsn_secret_id?: string;
  hmac_secret_id?: string;
  ssh_key_secret_id?: string;
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

/** Identifying metadata for a grantable secret; never its source or value. */
export interface AdminSecret {
  id: string;
  /** The grant column this kind uses, such as `static_secret_id`. */
  grant_key: string;
  foreign_id: string;
  name: string;
  description?: string | null;
  labels?: Record<string, unknown>;
  [field: string]: unknown;
}

/** A secret a principal holds, directly or through a role. */
export interface EffectiveGrant {
  id: string;
  grant_key: string;
  foreign_id: string;
  name?: string | null;
  direct: boolean;
  roles: { id: string; foreign_id: string; name: string }[];
  [field: string]: unknown;
}

/** A static, pg_dsn, gcp_auth, or gcp_id_token secret document; its source reference, never its value. */
export interface SecretDocument {
  id: string;
  foreign_id: string;
  name: string;
  description?: string | null;
  labels?: Record<string, unknown>;
  [field: string]: unknown;
}

/** The calling sandbox's redacted permissions. */
export interface SandboxPermissions {
  sandbox_id: string;
  proxy_id: string;
  principal_id: string;
  principal: { id: string; foreign_id: string; name: string };
  capabilities: Record<string, boolean>;
  slack_channel_permissions: SlackChannelPermission[];
  oauth_credentials: OAuthCredential[];
  connected_tools: string[];
  permissions: Record<string, unknown>;
  [field: string]: unknown;
}

/** The reachability check behind `health`. */
export interface HealthResult {
  ok: boolean;
  tool: string;
  error: string | null;
  details: {
    sandbox_id?: string;
    principal_id?: string;
    proxy_id?: string;
  };
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
  options: { method?: string; params?: Params; data?: unknown; label: string },
): Promise<any> {
  const res = await ctx.fetch(baseUrl(ctx) + path + query(options.params ?? {}), {
    method: options.method ?? "GET",
    headers: headers(ctx, options.data !== undefined),
    body: options.data === undefined ? undefined : JSON.stringify(options.data),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${options.label} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return text ? JSON.parse(text) : undefined;
}

function dataObject(payload: any, label: string): any {
  const data = payload && typeof payload === "object" ? payload.data : undefined;
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    throw new Error(`${label} response did not include a data object`);
  }
  return data;
}

function dataArray(payload: any, label: string): any[] {
  const data = payload && typeof payload === "object" ? payload.data : undefined;
  if (!Array.isArray(data)) throw new Error(`${label} response did not include a data array`);
  return data;
}

const sandbox = "centaur-console";
const admin = "centaur-console admin";

async function adminListAll(
  ctx: Ctx,
  path: string,
  extra: Record<string, string | number> = {},
): Promise<any[]> {
  const records: any[] = [];
  let page = 1;
  for (;;) {
    const payload = await request(ctx, path, {
      params: { ...extra, page, limit: ADMIN_PAGE_LIMIT },
      label: admin,
    });
    records.push(...dataArray(payload, admin));
    const totalPages = Number(payload?.meta?.total_pages ?? 0);
    if (!Number.isFinite(totalPages) || page >= totalPages) return records;
    page += 1;
  }
}

/**
 * A Console admin reference is an OID when it wears the OID prefix; otherwise
 * it is a foreign ID, which the console list finds with `q` and an exact match.
 */
async function adminFindRecord(
  ctx: Ctx,
  collection: string,
  ref: string,
  oidPrefix: string,
): Promise<any> {
  if (ref.startsWith(oidPrefix)) {
    return dataObject(
      await request(ctx, `${collection}/${encodeURIComponent(ref)}`, { label: admin }),
      admin,
    );
  }
  const matches = (await adminListAll(ctx, collection, { q: ref })).filter(
    (row) => row.id === ref || row.foreign_id === ref,
  );
  if (matches.length !== 1) {
    const found = matches.length === 0 ? "no record" : "several records";
    throw new Error(`${found} matched ${ref}; pass the OID`);
  }
  return matches[0];
}

async function adminPrincipalId(ctx: Ctx, principal: string): Promise<string> {
  return String((await adminFindRecord(ctx, ADMIN_PRINCIPALS_PATH, principal, "prn_")).id);
}

async function adminRoleId(ctx: Ctx, role: string): Promise<string> {
  return String((await adminFindRecord(ctx, ADMIN_ROLES_PATH, role, "role_")).id);
}

/** Resolve a secret by OID, foreign ID or name; names must be unambiguous. */
async function findSecret(ctx: Ctx, secret: string): Promise<AdminSecret> {
  const secrets: AdminSecret[] = await adminListAll(ctx, ADMIN_SECRETS_PATH);
  const matches = secrets.filter(
    (row) => secret === row.id || secret === row.foreign_id || secret === row.name,
  );
  if (matches.length !== 1) {
    const found = matches.length === 0 ? "no secret" : "several secrets";
    throw new Error(`${found} matched ${secret}; pass the secret OID`);
  }
  return matches[0];
}

/** The kind-specific document path for a writable secret. */
async function secretDocumentPath(ctx: Ctx, secret: string): Promise<string> {
  const match = await findSecret(ctx, secret);
  const kind = String(match.grant_key || "").replace(/_secret_id$/, "");
  const path = ADMIN_WRITABLE_SECRET_PATHS[kind];
  if (!path) throw new Error(`${secret} is a ${kind} secret; edit it in the Console UI`);
  return `${path}/${encodeURIComponent(match.id)}`;
}

async function grantSecret(ctx: Ctx, granteeKey: string, granteeId: string, secret: string): Promise<Grant> {
  const match = await findSecret(ctx, secret);
  return dataObject(
    await request(ctx, ADMIN_GRANTS_PATH, {
      method: "POST",
      data: { data: { [granteeKey]: granteeId, [match.grant_key]: match.id } },
      label: admin,
    }),
    admin,
  );
}

async function revokeSecret(ctx: Ctx, grantsPath: string, secret: string, grantee: string): Promise<void> {
  const match = await findSecret(ctx, secret);
  const grants: Grant[] = await adminListAll(ctx, grantsPath);
  const grant = grants.find((row) => (row as Record<string, unknown>)[match.grant_key] === match.id);
  if (!grant) throw new Error(`${secret} is not granted directly to ${grantee}`);
  await request(ctx, `${ADMIN_GRANTS_PATH}/${encodeURIComponent(grant.id)}`, {
    method: "DELETE",
    label: admin,
  });
}

/**
 * The calling sandbox's redacted permissions: sandbox, proxy and principal ids, the principal's
 * sandbox capabilities, Slack channel permissions, OAuth credentials, connected tool names and
 * the redacted iron-proxy config. Use it to find out what this sandbox may do before planning work.
 */
export async function permissions(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ permissions: SandboxPermissions }> {
  return {
    permissions: dataObject(await request(ctx, PERMISSIONS_PATH, { label: sandbox }), sandbox),
  };
}

/**
 * Enabled OAuth apps and their consent `start_url`, which a user must open in a browser to link an
 * account. `allowed_scopes` is what the app may request; `provider` such as `google` or `slack`.
 */
export async function oauth_apps(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ apps: OAuthApp[] }> {
  return { apps: dataArray(await request(ctx, OAUTH_APPS_PATH, { label: sandbox }), sandbox) };
}

/**
 * Scheduled tasks the caller owns: the current Console user's, or the current Slack channel's in a
 * channel thread. Each task carries its prompt, delivery destination, cron expression, timezone,
 * next run and last run outcome.
 */
export async function scheduled_tasks(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ tasks: ScheduledTask[] }> {
  return {
    tasks: dataArray(await request(ctx, SCHEDULED_TASKS_PATH, { label: sandbox }), sandbox),
  };
}

/** One owned scheduled task by OID (`tsk_...`). */
export async function scheduled_task(
  args: { task_id: string },
  ctx: Ctx,
): Promise<{ task: ScheduledTask }> {
  const path = `${SCHEDULED_TASKS_PATH}/${encodeURIComponent(args.task_id)}`;
  return { task: dataObject(await request(ctx, path, { label: sandbox }), sandbox) };
}

/**
 * Create a scheduled task owned by the caller. Omit `cron_expression` for a manual (run-on-demand)
 * task; otherwise pass five cron fields, interpreted in Pacific Time. Omit `delivery_channel` to
 * deliver to the caller's DM, or to the current channel in a Slack channel thread. `enabled`
 * defaults to true. Pass `owner: "me"` in a Slack channel thread to create the task for the person
 * who sent the current message instead of the channel: it runs with their permissions and always
 * delivers to their DM (any other `delivery_channel` is refused). It cannot target anyone else, and
 * such tasks are listed and managed from that person's DM, not the channel.
 */
export async function create_scheduled_task(
  args: {
    name: string;
    prompt: string;
    cron_expression?: string;
    delivery_channel?: string;
    enabled?: boolean;
    owner?: "me";
  },
  ctx: Ctx,
): Promise<{ task: ScheduledTask }> {
  const data: Record<string, unknown> = {
    name: args.name,
    prompt: args.prompt,
    cron_expression: args.cron_expression === undefined ? null : args.cron_expression,
    enabled: args.enabled === undefined ? true : args.enabled,
  };
  if (args.delivery_channel !== undefined) data.delivery_channel = args.delivery_channel;
  if (args.owner !== undefined) data.owner = args.owner;
  return {
    task: dataObject(
      await request(ctx, SCHEDULED_TASKS_PATH, {
        method: "POST",
        data: { data },
        label: sandbox,
      }),
      sandbox,
    ),
  };
}

/**
 * Update selected fields of an owned scheduled task; omitted fields are kept. Pass
 * `cron_expression: ""` to remove the schedule and leave a manual task. Channel tasks always keep
 * delivering to their channel.
 */
export async function update_scheduled_task(
  args: {
    task_id: string;
    name?: string;
    prompt?: string;
    cron_expression?: string;
    delivery_channel?: string;
    enabled?: boolean;
  },
  ctx: Ctx,
): Promise<{ task: ScheduledTask }> {
  const data: Record<string, unknown> = {};
  for (const key of ["name", "prompt", "cron_expression", "delivery_channel", "enabled"] as const) {
    const value = args[key];
    if (value !== undefined) data[key] = value;
  }
  if (Object.keys(data).length === 0) {
    throw new Error("at least one scheduled task field must be provided");
  }
  const path = `${SCHEDULED_TASKS_PATH}/${encodeURIComponent(args.task_id)}`;
  return {
    task: dataObject(await request(ctx, path, { method: "PATCH", data: { data }, label: sandbox }), sandbox),
  };
}

/** Delete an owned scheduled task by OID. */
export async function delete_scheduled_task(
  args: { task_id: string },
  ctx: Ctx,
): Promise<{ id: string; deleted: boolean }> {
  const path = `${SCHEDULED_TASKS_PATH}/${encodeURIComponent(args.task_id)}`;
  await request(ctx, path, { method: "DELETE", label: sandbox });
  return { id: args.task_id, deleted: true };
}

/**
 * Queue an immediate run of an owned, enabled scheduled task. Returns the task with `queued: true`;
 * the run itself starts asynchronously and can be followed in the session it creates.
 */
export async function run_scheduled_task(
  args: { task_id: string },
  ctx: Ctx,
): Promise<{ task: ScheduledTask }> {
  const path = `${SCHEDULED_TASKS_PATH}/${encodeURIComponent(args.task_id)}/run`;
  return { task: dataObject(await request(ctx, path, { method: "POST", label: sandbox }), sandbox) };
}

/**
 * List principals, with labels, sandbox capabilities and Slack channel permissions. Pass
 * `slack_channel_id` to select only principals bound to that Slack channel (C... or G...). Console
 * admins only.
 */
export async function admin_principals(
  args: { slack_channel_id?: string },
  ctx: Ctx,
): Promise<{ principals: Principal[] }> {
  if (args.slack_channel_id) {
    const channel = args.slack_channel_id.trim().toLowerCase();
    const records = await adminListAll(ctx, ADMIN_PRINCIPALS_PATH, { q: channel });
    return {
      principals: records.filter(
        (row) =>
          row.kind === "slack_channel" &&
          String(row.foreign_id || "").toLowerCase().endsWith(`-${channel}`),
      ),
    };
  }
  return { principals: await adminListAll(ctx, ADMIN_PRINCIPALS_PATH) };
}

/**
 * Create a principal for one Slack channel (C... or G...), one Slack user in the caller's
 * workspace, or one WORKFLOW_NAME. Pass exactly one of the three arguments; Console derives the
 * kind and foreign ID. Console admins only.
 */
export async function admin_create_principal(
  args: { slack_channel_id?: string; slack_user_id?: string; workflow_name?: string },
  ctx: Ctx,
): Promise<{ principal: Principal }> {
  const identities: [kind: string, key: string, value: string | undefined][] = [
    ["slack_channel", "slack_channel_id", args.slack_channel_id],
    ["slack_dm", "slack_user_id", args.slack_user_id],
    ["workflow", "workflow_name", args.workflow_name],
  ];
  const given = identities.filter(([, , value]) => value !== undefined);
  if (given.length !== 1) {
    throw new Error("pass exactly one of slack_channel_id, slack_user_id, or workflow_name");
  }
  const [kind, , value] = given[0];
  const data: Record<string, unknown> = { kind };
  if (kind === "slack_channel") {
    const channel = String(value).trim().toUpperCase();
    if (!/^[CG][A-Z0-9]{8,}$/.test(channel)) {
      throw new Error("slack_channel_id must be a C... or G... channel ID");
    }
    data.foreign_id = `slack-channel-${channel.toLowerCase()}`;
    data.name = `Slack Channel ${channel}`;
    data.slack_channel_id = channel;
    data.slack_channel_permissions = [
      {
        channel_id: channel,
        upload_enabled: true,
        download_enabled: true,
        history_enabled: true,
      },
    ];
  } else if (kind === "slack_dm") {
    const user = String(value).trim().toUpperCase();
    const current = dataObject(await request(ctx, PERMISSIONS_PATH, { label: sandbox }), sandbox);
    const foreignId = String(current.principal?.foreign_id || "");
    const match = /^slack-user-([^-]+)-/.exec(foreignId);
    if (!match) throw new Error("the current sandbox has no Slack team to create a DM principal in");
    const team = match[1].toUpperCase();
    data.foreign_id = `slack-user-${team.toLowerCase()}-${user.toLowerCase()}`;
    data.name = `Slack User ${user} (team ${team})`;
    data.slack_user_id = user;
    data.slack_team_id = team;
  } else {
    const name = String(value).trim();
    if (!name) throw new Error("workflow_name must be a WORKFLOW_NAME");
    const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "x";
    data.foreign_id = `workflow-${slug}`;
    data.name = `Workflow ${name}`;
    data.labels = { "managed-by": "centaur", workflow_name: name };
  }
  return {
    principal: dataObject(
      await request(ctx, ADMIN_PRINCIPALS_PATH, {
        method: "POST",
        data: { data },
        label: admin,
      }),
      admin,
    ),
  };
}

/**
 * Read one principal by OID (`prn_...`) or foreign ID, with everything `admin_principals` returns
 * plus its `roles` and its direct secret `grants`. Console admins only.
 */
export async function admin_principal(
  args: { principal: string },
  ctx: Ctx,
): Promise<{ principal: Principal; roles: Role[]; grants: Grant[] }> {
  const principal: Principal = await adminFindRecord(
    ctx,
    ADMIN_PRINCIPALS_PATH,
    args.principal,
    "prn_",
  );
  const path = `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principal.id)}`;
  return {
    principal,
    roles: dataArray(await request(ctx, `${path}/roles`, { label: admin }), admin),
    grants: await adminListAll(ctx, `${path}/grants`),
  };
}

/**
 * Secrets a principal holds directly or through its roles: `direct` is true for a direct grant and
 * `roles` lists the roles that carry it. Consult it when access to a secret is unclear.
 */
export async function admin_effective_grants(
  args: { principal: string },
  ctx: Ctx,
): Promise<{ grants: EffectiveGrant[] }> {
  const principalId = await adminPrincipalId(ctx, args.principal);
  const path = `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}/effective_grants`;
  return { grants: await adminListAll(ctx, path) };
}

/** Every role that can be assigned, with its labels and Slack channel permissions. */
export async function admin_roles(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ roles: Role[] }> {
  return { roles: await adminListAll(ctx, ADMIN_ROLES_PATH) };
}

/**
 * Assign a role to a principal. Both accept an OID or a foreign ID (for example `tool-github`).
 * Returns the role record. Console admins only.
 */
export async function admin_assign_role(
  args: { principal: string; role: string },
  ctx: Ctx,
): Promise<{ role: Role }> {
  const [principalId, roleId] = await Promise.all([
    adminPrincipalId(ctx, args.principal),
    adminRoleId(ctx, args.role),
  ]);
  const path = `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}/roles`;
  return {
    role: dataObject(
      await request(ctx, path, { method: "POST", data: { data: { role_id: roleId } }, label: admin }),
      admin,
    ),
  };
}

/** Remove a role (OID or foreign ID) from a principal (OID or foreign ID). Console admins only. */
export async function admin_unassign_role(
  args: { principal: string; role: string },
  ctx: Ctx,
): Promise<{ principal: string; role: string; removed: boolean }> {
  const [principalId, roleId] = await Promise.all([
    adminPrincipalId(ctx, args.principal),
    adminRoleId(ctx, args.role),
  ]);
  const path =
    `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}/roles/${encodeURIComponent(roleId)}`;
  await request(ctx, path, { method: "DELETE", label: admin });
  return { principal: args.principal, role: args.role, removed: true };
}

/**
 * List grantable secrets of every kind with id, `grant_key`, foreign ID, name and labels, never
 * their source or value. `query` filters by name or foreign ID substring. Console admins only.
 */
export async function admin_secrets(
  args: { query?: string },
  ctx: Ctx,
): Promise<{ secrets: AdminSecret[] }> {
  return {
    secrets: await adminListAll(
      ctx,
      ADMIN_SECRETS_PATH,
      args.query ? { q: args.query } : {},
    ),
  };
}

/**
 * Read a static, pg_dsn, gcp_auth, or gcp_id_token secret's document by OID, foreign ID or unambiguous name:
 * its source reference and rules, never the secret value. Other secret kinds can only be inspected
 * in the Console UI.
 */
export async function admin_secret(
  args: { secret: string },
  ctx: Ctx,
): Promise<{ secret: SecretDocument }> {
  const path = await secretDocumentPath(ctx, args.secret);
  return { secret: dataObject(await request(ctx, path, { label: admin }), admin) };
}

/**
 * Create a static, pg_dsn, gcp_auth, or gcp_id_token secret from a Console API document. `data` is
 * that kind's document, for example `{ name, foreign_id, description, labels, source: {
 * source_type, config } }` for static, `{ name, foreign_id, database, role, dsn: {...} }` for
 * pg_dsn, `{ name, foreign_id, scopes, rules, keyfile: {...} }` or `{ ..., credentials_provider: {
 * type: "workload_identity" } }` for gcp_auth, or `{ name, foreign_id, audience, keyfile: {...} }`
 * for gcp_id_token. Sources must reference an external secret store. Console admins only.
 */
export async function admin_create_secret(
  args: {
    kind: "static" | "pg_dsn" | "gcp_auth" | "gcp_id_token";
    data: Record<string, unknown>;
  },
  ctx: Ctx,
): Promise<{ secret: SecretDocument }> {
  const path = ADMIN_WRITABLE_SECRET_PATHS[args.kind];
  if (!path) throw new Error("kind must be static, pg_dsn, gcp_auth, or gcp_id_token");
  return {
    secret: dataObject(
      await request(ctx, path, { method: "POST", data: { data: args.data }, label: admin }),
      admin,
    ),
  };
}

/**
 * Replace the given fields of a static, pg_dsn, gcp_auth, or gcp_id_token secret, addressed by OID,
 * foreign ID or unambiguous name; omitted fields, including its source, are kept. Console admins
 * only.
 */
export async function admin_update_secret(
  args: { secret: string; data: Record<string, unknown> },
  ctx: Ctx,
): Promise<{ secret: SecretDocument }> {
  const path = await secretDocumentPath(ctx, args.secret);
  return {
    secret: dataObject(
      await request(ctx, path, { method: "PATCH", data: { data: args.data }, label: admin }),
      admin,
    ),
  };
}

/**
 * Grant a secret (OID, foreign ID or unambiguous name) directly to a principal (OID or foreign ID).
 * Returns the grant record; repeat grants are idempotent. Console admins only.
 */
export async function admin_grant_secret(
  args: { principal: string; secret: string },
  ctx: Ctx,
): Promise<{ grant: Grant }> {
  const principalId = await adminPrincipalId(ctx, args.principal);
  return { grant: await grantSecret(ctx, "principal_id", principalId, args.secret) };
}

/**
 * Revoke a secret granted directly to a principal. A secret that arrives only through a role stays
 * granted; use `admin_revoke_role_secret` on that role instead. Console admins only.
 */
export async function admin_revoke_secret(
  args: { principal: string; secret: string },
  ctx: Ctx,
): Promise<{ principal: string; secret: string; revoked: boolean }> {
  const principalId = await adminPrincipalId(ctx, args.principal);
  const path = `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}/grants`;
  await revokeSecret(ctx, path, args.secret, args.principal);
  return { principal: args.principal, secret: args.secret, revoked: true };
}

/**
 * Grant a secret to a role (OID or foreign ID), and so to every principal holding that role.
 * Returns the grant record. Console admins only.
 */
export async function admin_grant_role_secret(
  args: { role: string; secret: string },
  ctx: Ctx,
): Promise<{ grant: Grant }> {
  const roleId = await adminRoleId(ctx, args.role);
  return { grant: await grantSecret(ctx, "role_id", roleId, args.secret) };
}

/** Revoke a secret granted to a role, and so from every principal holding it. Console admins only. */
export async function admin_revoke_role_secret(
  args: { role: string; secret: string },
  ctx: Ctx,
): Promise<{ role: string; secret: string; revoked: boolean }> {
  const roleId = await adminRoleId(ctx, args.role);
  const path = `${ADMIN_ROLES_PATH}/${encodeURIComponent(roleId)}/grants`;
  await revokeSecret(ctx, path, args.secret, args.role);
  return { role: args.role, secret: args.secret, revoked: true };
}

/**
 * Create or update a principal's Slack channel permission for one channel (`C...`). `upload`,
 * `download` and `history` each default to enabled when omitted on a new permission and stay
 * unchanged when omitted on an existing one. Console admins only.
 */
export async function admin_set_slack_permission(
  args: {
    principal: string;
    channel_id: string;
    upload?: boolean;
    download?: boolean;
    history?: boolean;
  },
  ctx: Ctx,
): Promise<{ permission: SlackChannelPermission }> {
  const principalId = await adminPrincipalId(ctx, args.principal);
  const principal = dataObject(
    await request(ctx, `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}`, {
      label: admin,
    }),
    admin,
  );
  const channel = String(args.channel_id).trim().toUpperCase();
  const existing = Array.isArray(principal.slack_channel_permissions)
    ? principal.slack_channel_permissions
    : [];
  const previous = existing.find(
    (row: SlackChannelPermission) => String(row.channel_id).toUpperCase() === channel,
  );
  const permission: SlackChannelPermission = {
    channel_id: channel,
    upload_enabled: args.upload ?? previous?.upload_enabled ?? true,
    download_enabled: args.download ?? previous?.download_enabled ?? true,
    history_enabled: args.history ?? previous?.history_enabled ?? true,
  };
  const rows = existing
    .filter(
      (row: SlackChannelPermission) => String(row.channel_id).toUpperCase() !== channel,
    )
    .map((row: SlackChannelPermission) => ({
      channel_id: row.channel_id,
      upload_enabled: row.upload_enabled,
      download_enabled: row.download_enabled,
      history_enabled: row.history_enabled,
    }));
  rows.push(permission);
  const payload = await request(
    ctx,
    `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}/slack_channel_permissions`,
    { method: "PUT", data: { data: { slack_channel_permissions: rows } }, label: admin },
  );
  const saved = dataArray(payload, admin).find(
    (row: SlackChannelPermission) => String(row.channel_id).toUpperCase() === channel,
  );
  if (!saved) throw new Error(`Console did not save the ${channel} permission`);
  return { permission: saved };
}

/**
 * Remove a principal's own permission for a Slack channel; a permission inherited from one of its
 * roles is unaffected. Console admins only.
 */
export async function admin_remove_slack_permission(
  args: { principal: string; channel_id: string },
  ctx: Ctx,
): Promise<{ principal: string; channel_id: string; removed: boolean }> {
  const principalId = await adminPrincipalId(ctx, args.principal);
  const path =
    `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}/slack_channel_permissions/` +
    encodeURIComponent(args.channel_id);
  await request(ctx, path, { method: "DELETE", label: admin });
  return { principal: args.principal, channel_id: args.channel_id, removed: true };
}

/**
 * Change selected sandbox capabilities of a principal (OID or foreign ID); unspecified ones are
 * unchanged. `repo_cache` is `none`, `public`, or `all`; `observability` allows logs and metrics;
 * `sessions_read` and `workflows_read` allow reading other sessions and workflow state;
 * `workflows_write` allows starting workflows. Console admins only.
 */
export async function admin_set_sandbox_access(
  args: {
    principal: string;
    repo_cache?: "none" | "public" | "all";
    observability?: boolean;
    sessions_read?: boolean;
    workflows_read?: boolean;
    workflows_write?: boolean;
  },
  ctx: Ctx,
): Promise<{ principal: Principal }> {
  const data: Record<string, unknown> = {};
  for (const [key, value] of [
    ["sandbox_repo_cache", args.repo_cache],
    ["sandbox_observability_enabled", args.observability],
    ["sandbox_sessions_read_enabled", args.sessions_read],
    ["sandbox_workflows_read_enabled", args.workflows_read],
    ["sandbox_workflows_write_enabled", args.workflows_write],
  ] as const) {
    if (value !== undefined) data[key] = value;
  }
  if (Object.keys(data).length === 0) {
    throw new Error("at least one sandbox capability must be provided");
  }
  const principalId = await adminPrincipalId(ctx, args.principal);
  const path = `${ADMIN_PRINCIPALS_PATH}/${encodeURIComponent(principalId)}`;
  return {
    principal: dataObject(
      await request(ctx, path, { method: "PATCH", data: { data }, label: admin }),
      admin,
    ),
  };
}

function adminTeamIds(payload: any): string[] {
  const data = dataObject(payload, admin);
  const ids = data.slack_external_team_ids;
  if (!Array.isArray(ids)) throw new Error(`${admin} response did not include team ids`);
  return ids.map(String);
}

async function adminReplaceSlackExternalTeams(
  ctx: Ctx,
  team_ids: string[],
): Promise<string[]> {
  const payload = await request(ctx, ADMIN_SLACK_EXTERNAL_TEAMS_PATH, {
    method: "PUT",
    data: { data: { slack_external_team_ids: team_ids } },
    label: admin,
  });
  return adminTeamIds(payload);
}

/** Slack Connect workspace (T...) and Enterprise Grid org (E...) ids Centaur accepts messages from. */
export async function admin_slack_external_teams(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ team_ids: string[] }> {
  const payload = await request(ctx, ADMIN_SLACK_EXTERNAL_TEAMS_PATH, { label: admin });
  return { team_ids: adminTeamIds(payload) };
}

/**
 * Allow messages from a Slack Connect workspace (T...) or Enterprise Grid org (E...). Returns the
 * updated allowlist. Console admins only.
 */
export async function admin_add_slack_external_team(
  args: { team_id: string },
  ctx: Ctx,
): Promise<{ team_ids: string[] }> {
  const team_id = args.team_id.trim().toUpperCase();
  const current = adminTeamIds(
    await request(ctx, ADMIN_SLACK_EXTERNAL_TEAMS_PATH, { label: admin }),
  );
  return {
    team_ids: current.includes(team_id)
      ? current
      : await adminReplaceSlackExternalTeams(ctx, [...current, team_id]),
  };
}

/**
 * Stop accepting messages from a Slack Connect workspace or org. Returns the updated allowlist.
 * Console admins only.
 */
export async function admin_remove_slack_external_team(
  args: { team_id: string },
  ctx: Ctx,
): Promise<{ team_ids: string[] }> {
  const team_id = args.team_id.trim().toUpperCase();
  const current = adminTeamIds(
    await request(ctx, ADMIN_SLACK_EXTERNAL_TEAMS_PATH, { label: admin }),
  );
  if (!current.includes(team_id)) {
    throw new Error(`${team_id} is not an allowed Slack Connect workspace`);
  }
  return {
    team_ids: await adminReplaceSlackExternalTeams(
      ctx,
      current.filter((id) => id !== team_id),
    ),
  };
}

/**
 * Check that the sandbox permissions endpoint is reachable and authorized for this sandbox, and
 * report its sandbox, principal and proxy ids. Never throws: failures come back as `ok: false`
 * with the error message.
 */
export async function health(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<HealthResult> {
  try {
    const body = await request(ctx, PERMISSIONS_PATH, { label: sandbox });
    const data = dataObject(body, sandbox);
    return {
      ok: true,
      tool: "centaur-console",
      error: null,
      details: {
        sandbox_id: data.sandbox_id,
        principal_id: data.principal_id,
        proxy_id: data.proxy_id,
      },
    };
  } catch (err) {
    return {
      ok: false,
      tool: "centaur-console",
      error: err instanceof Error ? err.message : String(err),
      details: {},
    };
  }
}
