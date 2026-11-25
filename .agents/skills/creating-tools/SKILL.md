---
name: creating-tools
description: "Scaffold and build new Code Mode connectors in connectors/. Use when asked to create a new tool or connector, add an API integration, or build a new client for an external service."
---

# Creating Connectors

New integrations are Code Mode connectors, not Python CLIs under `tools/`. A
connector is a directory `connectors/<name>/` holding a `connector.toml` and an
`index.ts`; every exported async function in `index.ts` is a tool. Agents find
and call them through `centaur-codemode search`, `load` and `exec` (the MCP
tools `centaur_catalog_search`, `centaur_catalog_load` and `centaur_execute`),
writing one JavaScript cell that calls `tools.<name>.<fn>(args)`.

Read `crates/centaur-codemode/README.md` first: it is the contract for the
runtime, `ctx`, files, websockets, limits and security invariants. Read
`connectors/centaur.d.ts` for the `Ctx`, `Response`, `RequestInit` and `Files`
types. Copy the shape of a recent connector such as `connectors/pangram`
(small) or `connectors/luma` (pagination plus a generic `request` escape hatch)
rather than inventing a new layout.

## Layout

```
connectors/<name>/
├── connector.toml   # description and secrets
└── index.ts         # exported async functions are the tools
connectors/_lib/     # helpers shared by connectors (http.ts, files.ts, ...)
```

- `<name>` is snake_case (`google_docs`, `token_terminal`). It is the name a
  principal's tool allowlist/blocklist matches exactly.
- Discovery is automatic: the Code Mode image copies `connectors/`, and the
  runtime reloads connector roots on change. There is no registry to edit.
- Put organization-specific connectors in an overlay repo
  (`overlays.sources[*].connectorsSubdir`, default `connectors`; see
  `docs/pages/extend/overlay.mdx`). A later root shadows an earlier one.

## connector.toml

Same schema as a tool's `[tool.centaur]` block (`docs/pages/extend/tools.mdx`
is the full secret reference): `description`, `secrets`, `optional_secrets`.

```toml
description = "<Name>: <what it reads and writes, in one line>"

secrets = [
    {type = "http", name = "<NAME>_API_KEY", mode = "inject", inject_header = "x-api-key", hosts = ["api.example.com"]},
]
```

- The description feeds catalog search, so name the nouns and verbs an agent
  would search for.
- Prefer `mode = "inject"`: iron-proxy adds the header on the wire and the
  connector needs no placeholder. Use `inject_formatter = "Bearer {{ .Value }}"`
  for bearer tokens. Give each secret the narrowest `hosts` list.
- Omit `secrets` for public APIs.

## index.ts

```ts
import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// <Name> API (api.example.com). Responses are passed through unchanged.

const BASE = "https://api.example.com";

/** An item. Lists the fields worth knowing; the API may send more. */
export interface Item {
  id: string;
  name?: string;
  [field: string]: unknown;
}

/** A page of items; pass `next_cursor` back as `cursor` for the next page. */
export interface ItemList {
  items: Item[];
  next_cursor?: string | null;
  [field: string]: unknown;
}

async function request(ctx: Ctx, path: string, init?: { method?: string; body?: unknown }): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it may come from the env.
  if (ctx.env.EXAMPLE_API_KEY) headers["x-api-key"] = ctx.env.EXAMPLE_API_KEY;
  if (init?.body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(`${BASE}${path}`, {
    method: init?.method ?? "GET",
    headers,
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
  });
  return readJson(res, "<Name>");
}

/**
 * Search items by `query`. Returns up to `limit` (default 10) items and a `next_cursor`
 * for the next page.
 */
export async function search_items(
  args: { query: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<ItemList> {
  return request(ctx, `/items${query({ q: args.query, limit: args.limit ?? 10, cursor: args.cursor })}`);
}
```

Rules the loader and type checker enforce:

- Each export has the signature `(args, ctx: Ctx)` with explicit parameter and
  return types (`isolatedDeclarations`) and a JSDoc comment, which becomes the
  description in the brief. Tools with no arguments take
  `_args: Record<string, never>`.
- Imports are limited to `import type ... from "../centaur"` and
  `../_lib/<name>`. Anything else fails at load. There is no npm, no Node API
  and no global `fetch`; use `ctx.fetch`, `ctx.websocket`, `ctx.sleep`,
  `ctx.base64`, `ctx.gzip`/`gunzip` and `ctx.files`.
- Declare exported types in the connector itself; the brief shows each
  connector's declarations alone.
- Return objects, not bare arrays.

Conventions:

- Return upstream data as the API sends it. Unwrap envelopes and turn API
  errors into thrown errors (`readJson` does both), but don't rename or coerce
  fields. Interfaces use the vendor's field names and end with
  `[field: string]: unknown`.
- Credentials never live in code or `ctx.env` in a deployment; `ctx.env` holds
  configuration (base URLs) and, for replace-mode secrets, placeholders. Let a
  base URL be overridden from `ctx.env` when the API has regional or
  self-hosted endpoints.
- Functions that produce or consume files take workspace paths and use
  `requireFiles`, `downloadTo`, `saveBytes` and `multipart` from
  `_lib/files.ts`. Never pass file contents as base64.
- Keep error messages short. `readJson` includes up to 1000 characters of the
  body; don't add response headers.
- Write JSDoc for an agent: say what each argument means, defaults, units,
  pagination and terminal states for polling (see `pangram.get_task`). Prefer
  a few well-documented functions plus a generic `request` escape hatch over
  wrapping every endpoint.
- Put a helper in `_lib/` only when a second connector needs it.

## Checks

```bash
pnpm install --frozen-lockfile
pnpm --filter @centaur/connectors lint
pnpm --filter @centaur/connectors typecheck
cargo test -p centaur-codemode      # includes compiling every repository connector
```

Then confirm the catalog sees it and a real call works:

```bash
cargo build -p centaur-codemode
B=target/debug/centaur-codemode
$B list | grep <name>
$B search "<words an agent would use>"
$B load <name>                      # check the brief reads well
$B exec --tools <name> <<'EOF'
return await tools.<name>.search_items({ query: "test", limit: 1 });
EOF
```

For a local `exec` against the real API, put the key in the environment from
the configured secret source (never on the command line or in a file in the
repo). Inspect `calls[].requests` in the result to confirm the method, URL and
status of each upstream request.

## Credentials and verification

1. Store the secret in the deployment's configured secret source under the
   exact `name` from `connector.toml` (see `docs/pages/secrets/`).
2. Create the matching secret in the Console (same name, type, inject header
   or formatter, and hosts) and grant it to the principal or a
   `tool-<name>` role. Use the `centaur-console-permissions` skill for the
   `centaur_console` connector calls, or the Console UI.
3. From a real sandbox for that principal, run `centaur-codemode load <name>`
   and one real `exec` call. Configuration alone does not prove access works;
   a 401 or 403 usually means the secret's name, hosts or inject settings
   don't match `connector.toml` (see the `auth-failure-log-triage` skill).

If the connector is missing from `list`, check that the directory name matches
the tool filter, the connector root is in `CENTAUR_CODEMODE_CONNECTORS`, and
`centaur-codemode list` with `RUST_LOG=info` reports no load error.
