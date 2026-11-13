# centaur-codemode

Experimental Code Mode runtime. An agent writes one JavaScript cell; the cell
calls TypeScript connectors (`connectors/<name>/index.ts`) in QuickJS and
returns one JSON value. One cell replaces a chain of single-tool MCP turns.

Status: spike. Not wired into api-rs, MCP, the chart or any image yet.

## Try it

```bash
cargo build -p centaur-codemode
B=target/debug/centaur-codemode

$B brief                                   # typed catalog an agent reads
$B exec <<'EOF'                            # run a cell, print the JSON response
const [orgs, chans] = await Promise.allSettled([
  tools.crunchbase.search_organizations({ query: "stripe", limit: 3 }),
  tools.slack.channels({ readable_only: true }),
]);
return { orgs, chans };
EOF
$B serve --listen 127.0.0.1:8790           # HTTP API
```

`--connectors a:b` sets connector roots (later roots shadow earlier ones;
default `connectors`) and `--tools slack,crunchbase` limits what a cell sees.
There is no principal argument: which principal a runner serves, and so which
credentials iron-proxy injects, is set by where it runs. `--context-file`
(default `$CENTAUR_SANDBOX_CONTEXT_FILE` or `~/.centaur/sandbox.json`) is the
sandbox context file, read once per cell and given to connectors as
`ctx.context` (and to cells as `context`) for routing decisions such as Slack's
direct or Centaur path.
Connectors read configuration from the
environment as `ctx.env` (`CENTAUR_API_URL`, ...). Behind iron-proxy,
credentials are injected on the wire; for local testing only, a connector may
read a real key from the environment (`CRUNCHBASE_API_KEY`, `SLACK_BOT_TOKEN`).
`HTTPS_PROXY` is honored; `--ca-file` adds the proxy CA.

## Cells

A cell is the body of an async function; its return value is the result.

| Global | Meaning |
|---|---|
| `tools.<connector>.<fn>(args)` | Call a connector function; the runtime passes `ctx`. |
| `context` | The calling context (sandbox context file), as connectors see it in `ctx.context`; `null` outside a session. |
| `console.log`, `setTimeout`, `sleep`, `TextEncoder`/`TextDecoder` | As usual; logs are returned. |

Cells get no `fetch` and no module loader: the network is reachable only
through connectors.

## Security invariants

1. Rust is the only trust boundary. The call record, policy checks and limits
   live in the host; nothing a JS realm reports is trusted.
2. Every host function a realm receives is safe for code in that realm to call
   with any arguments. Hiding a binding is tidiness, not protection.
3. Cells and connectors never share a realm. Every tool call runs in a fresh
   QuickJS runtime; arguments and results cross as JSON text. Cell realms
   have no module loader; connector realms can import only shared `_lib`
   modules.
4. A connector's `fetch` is bound to its call, so each upstream request is
   recorded against the call that made it, at the point of I/O (method, URL
   without query string, status, duration).
5. `invoke` checks every tool against the principal's allowlist; refusals are
   recorded.
6. Time, memory, call count and call concurrency are bounded per cell,
   including work done inside connectors.

Deployment invariant: the runner's environment holds no real credentials, only
configuration (API base URLs, database proxy endpoints) and placeholders that
iron-proxy swaps on the wire. Connectors read it as `ctx.env`; cells cannot.
Egress is limited to the principal's iron-proxy.

## Connectors

```text
connectors/<name>/connector.toml   description + secrets (same schema as [tool.centaur])
connectors/<name>/index.ts         every exported async function is a tool
connectors/_lib/<name>.ts          helpers shared by connectors (not by cells)
connectors/centaur.d.ts            Ctx, Response, CallingContext types
```

Connectors may import only `../centaur` (with `import type`) and
`../_lib/<name>`; shared modules may import each other as `./<name>`. Anything
else fails at load. Exported signatures should use types declared in the
connector itself, since the brief shows each connector's declarations alone.

Exported functions take `(args, ctx: Ctx)`, need explicit parameter and return
types (TypeScript `isolatedDeclarations`), and a JSDoc comment, which becomes
the description in the brief. Return objects, not bare arrays.

Return upstream data as the API sends it. Unwrap envelopes and turn API errors
into thrown errors, but don't coerce or rename fields: types describe the
vendor's shape, using its field names, so drift shows up as real data rather
than silent nulls.
Type-check with `npx -p typescript tsc -p connectors`.

## HTTP API

| Route | Body / query | Result |
|---|---|---|
| `POST /exec` | `{code, tools?}` | `{ok, result?, error?: {message, stack?, tool?}, logs, calls: [{tool, ok, error?, ms, requests: [{method, url, status?, error?, ms}]}], ms}` |
| `GET /catalog?tools=a,b` | | `{connectors, brief}` |
| `GET /healthz` | | `{ok, connectors}` |

The calling context is informational: the agent can write the file in a
sandbox, so connectors may route on it but nothing authorizes from it.

## Limits (hard-coded)

Cell 120 s (busy loops are interrupted), tool call 60 s, 64 MiB heap and 1 MiB
stack per realm (the cell and each running call), 1000 calls and 8 concurrent
calls per cell, 10 MiB result, 10 MiB per HTTP response.

## Cost of a realm per call

Connectors and the preludes are compiled to QuickJS bytecode once; each call
creates a runtime and context, loads both modules and runs on its own task.
Release build, x86_64:

| | Shared realm | Realm per call |
|---|---|---|
| 100 sequential no-I/O calls in one cell | 6 ms | about 62 ms (0.6 ms/call) |
| 100 parallel no-I/O calls in one cell | 6 ms | about 14 ms |
| 6 parallel calls to a local mock API | 13.7 ms p50 | 6.7 ms p50 |

Of the 0.6 ms, creating a context with every intrinsic is about 0.15 ms and
loading and evaluating connector bytecode about 0.2 ms. Against upstream APIs
that take 50–500 ms the difference is noise.
