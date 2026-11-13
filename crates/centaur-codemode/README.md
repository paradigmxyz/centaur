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
$B search funding rounds by investor       # BM25 over connector functions
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
default `connectors`), `--tools slack,crunchbase` limits what a cell sees, and
`exec --oauth slack` marks the principal as holding a live Slack OAuth
credential (direct Slack access). Credentials come from the environment
(`CRUNCHBASE_API_KEY`, `SLACK_BOT_TOKEN`, `CENTAUR_API_URL`, ...) or, behind
iron-proxy, are injected on the wire. `HTTPS_PROXY` is honored;
`--ca-file` adds the proxy CA.

## Cells

A cell is the body of an async function; its return value is the result.

| Global | Meaning |
|---|---|
| `tools.<connector>.<fn>(args)` | Call a connector function; the runtime passes `ctx`. |
| `files.read/readText/write` | The principal's scratch directory (relative paths only). |
| `console.log`, `setTimeout`, `sleep`, `TextEncoder`/`TextDecoder` | As usual; logs are returned. |

Cells get no `fetch`: the network is reachable only through connectors.

## Connectors

```text
connectors/<name>/connector.toml   description + secrets (same schema as [tool.centaur])
connectors/<name>/index.ts         every exported async function is a tool
connectors/centaur.d.ts            Ctx, Response, Files, Principal types
```

Exported functions take `(args, ctx: Ctx)`, need explicit parameter and return
types (TypeScript `isolatedDeclarations`), and a JSDoc comment, which becomes
the description in the brief and search. Return objects, not bare arrays.
Type-check with `npx -p typescript tsc -p connectors`.

## HTTP API

| Route | Body / query | Result |
|---|---|---|
| `POST /exec` | `{code, principal: {id, email?, name?, oauth[]}, tools?}` | `{ok, result?, error?: {message, stack?, tool?}, logs, calls, ms}` |
| `GET /catalog?tools=a,b` | | `{connectors, brief}` |
| `POST /search` | `{query, limit?, tools?}` | `{results: [{tool, signature, doc, score}]}` |
| `GET`/`PUT /files/{path}` | bytes | bytes / `{path, bytes}` |
| `GET /healthz` | | `{ok, connectors}` |

`serve --principal <id>` refuses `/exec` for any other principal.

## Limits (hard-coded)

Cell 120 s (busy loops are interrupted), tool call 60 s, 64 MiB heap and 1 MiB
stack per cell, 10 MiB result, 10 MiB per HTTP response, 100 MiB per file.
Each cell runs in a fresh QuickJS runtime and context.
