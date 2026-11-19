# centaur-codemode

Experimental Code Mode runtime. An agent writes one JavaScript cell; the cell
calls TypeScript connectors (`connectors/<name>/index.ts`) in QuickJS and
returns one JSON value. One cell replaces a chain of single-tool MCP turns.

Status: experimental. `serve` is the HTTP API api-rs talks to when
serving `/mcp` (on unless `CENTAUR_MCP_ENABLED=false`); the CLI is for local work and tests.

## Try it

```bash
cargo build -p centaur-codemode
B=target/debug/centaur-codemode

$B list                                    # one line per connector
$B search "slack channel history"          # rank functions, with signatures
$B load slack                              # one connector's declarations
$B brief                                   # typed catalog an agent reads
$B exec <<'EOF'                            # run a cell, print the JSON response
const [orgs, chans] = await Promise.allSettled([
  tools.crunchbase.search_organizations({ query: "stripe", limit: 3 }),
  tools.slack.channels({ readable_only: true }),
]);
return { orgs, chans };
EOF
$B serve --listen 127.0.0.1:8790
curl -s localhost:8790/healthz
curl -s localhost:8790/search -d '{"query": "search organizations"}'
```

`--connectors a:b` sets connector roots (later roots shadow earlier ones;
default `connectors`). A root that is not mounted is skipped (logged at `info`), so
overlays a principal cannot see do not break startup; loading with no connectors
at all is an error. `--tools slack,crunchbase` limits what a cell sees; the HTTP
API sends a per-request policy instead. Every CLI command also applies the
deployment filter a Centaur sandbox sets, `TOOL_ALLOWLIST` and
`TOOL_BLOCKLIST` (comma-separated connector names, as api-rs sends them to
`serve`); `--tools` narrows that allowlist and never widens it.
`list`, `search` and `load` are the CLI forms of the MCP tools
`centaur_catalog_search` and `centaur_catalog_load`, so a session sandbox finds
connectors the same way an MCP client does. Session sandboxes ship the binary
and connectors.
Commands other than `serve` log only warnings and errors by default, since
their stderr lands in an agent's context; `RUST_LOG` overrides.
TLS uses the system trust store and honors `SSL_CERT_FILE`, which a sandbox
points at the proxy CA; `--ca-file` adds roots on top.
There is no principal argument: which principal a runner serves, and so which
credentials iron-proxy injects, is set by where it runs.

`context` (`ctx.context` for connectors) is the calling context connectors
route on, such as Slack's direct or Centaur path. A session sandbox reads the
file harness-server writes (`--context-file`, default `~/.centaur/sandbox.json`).
A Code Mode sandbox has none, so `serve` builds the same shape from Console's
`GET /api/v1/sandbox/permissions` through iron-proxy, with `harness: "codemode"`
and a null `thread_key`. It is preloaded at startup and refreshed in the
background (60 s after a success, 5 s after a failure); a cell only reads the
latest snapshot, a failed refresh keeps the last good one, and until the first
success the context is `sandbox: null` with a `sandbox_error`.
Connectors read configuration from the
environment as `ctx.env` (`CENTAUR_API_URL`, ...). Behind iron-proxy,
credentials are injected on the wire; for local testing only, a connector may
read a real key from the environment (`CRUNCHBASE_API_KEY`, `SLACK_BOT_TOKEN`).
`HTTPS_PROXY` is honored; `--ca-file` adds the proxy CA.
`--files-dir` (`$CENTAUR_CODEMODE_FILES_DIR`) turns on the files capability
(below); without it connectors have no file access.

## Packaging

A dedicated slim image, built from `services/codemode/Dockerfile`, runs Code
Mode sandboxes and is configured by `codemode.image` /
`CENTAUR_CODEMODE_IMAGE` (default `centaur-codemode:latest`). It
contains only the `centaur-codemode` binary at
`/usr/local/bin/centaur-codemode` and the repository connectors at
`/opt/centaur/connectors`, with `CENTAUR_CODEMODE_CONNECTORS` defaulting to
that root. api-rs mounts overlay connectors under `/home/agent/github` and
appends them to the same variable. The image runs as `agent` (uid/gid 1001),
has no `/entrypoint.sh`, and its entrypoint is `centaur-codemode` with the
default command `serve`.

## Cells

A cell is the body of an async function; its return value is the result.

| Global | Meaning |
|---|---|
| `tools.<connector>.<fn>(args)` | Call a connector function; the runtime passes `ctx`. |
| `context` | The calling context (sandbox context file, or Console permissions in Code Mode), as connectors see it in `ctx.context`; `null` when there is none. |
| `console.log`, `setTimeout`, `sleep`, `TextEncoder`/`TextDecoder`, `atob`/`btoa` | As usual; logs are returned. |

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
4. A connector's `fetch` and `websocket` are bound to its call, so each
   upstream request and socket is recorded against the call that made it, at
   the point of I/O (method, URL without query string, status, duration), and
   every socket closes when its call ends. File operations are recorded the
   same way (operation, path, bytes).
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
connectors/<name>/connector.toml   description and secrets
connectors/<name>/index.ts         every exported async function is a tool
connectors/_lib/<name>.ts          helpers shared by connectors (not by cells)
connectors/centaur.d.ts            Ctx, Response, Files, CallingContext types
```

`connector.toml` uses the same schema as the tool host's `[tool.centaur]`:
`description`, `secrets` and `optional_secrets`. The connector's directory name
is the name a principal's tool filter matches.

Connectors may import only `../centaur` (with `import type`) and
`../_lib/<name>`; shared modules may import each other as `./<name>`. Anything
else fails at load. Exported signatures should use types declared in the
connector itself, since the brief shows each connector's declarations alone.

Besides `fetch`, `env`, `context` and `sleep`, `ctx` offers `base64`
(`encode`, `decode`, `decodeText`; both alphabets) and `gzip`/`gunzip` (gzip
or zlib, output capped like a response body). `fetch` already decodes
`Content-Encoding: gzip` and `deflate` responses.

### WebSockets

`ctx.websocket(url, { timeout_ms? })` opens a `ws://` or `wss://` socket for
text messages with an HTTP/1.1 upgrade through the same proxy and trust roots
as `fetch`, so iron-proxy sees the handshake. It returns `{ send, recv, close }`:
`recv(timeout_ms?)` returns the next message, `null` when none arrives in time
(default: the call's remaining time), and throws once the socket has closed.

A socket belongs to its call: the host closes whatever the call left open when
the call ends, so state that must outlive a call (a remote browser's tabs, for
example) has to live upstream. A call may make 4 connection attempts; a
message is at most 10 MiB either way. Each attempt is a `requests` entry with
`method: "WS"` and status 101 on success. `_lib/cdp.ts` speaks the Chrome
DevTools Protocol over it; `connectors/browser_use` uses that to drive Browser
Use cloud browsers.

### Files

With `--files-dir`, connectors get `ctx.files`, rooted at that directory: in a
Centaur sandbox, the agent's workspace. Without it `ctx.files` is `null`.
Cells never get file access; they pass paths between tools.

- `read`, `readText`, `write`, `stat`. Paths are relative to the root or
  absolute inside it, and must stay inside it after symlinks resolve. Writes
  create parent directories and land atomically (temporary file, then
  rename), so they never follow a symlink at the destination.
- `download(url, path, init?)` streams a 2xx body to disk in the host, past the
  10 MiB response limit and without the bytes entering the realm. A non-2xx
  response saves nothing and returns the start of its body.
- Transfers can take a while: `RequestInit.timeout_ms` (up to 10 minutes)
  sets one request's timeout, body included, and pushes back the call's and
  the cell's deadlines to fit. `downloadTo` defaults to 5 minutes.
- A `fetch` body may be a list of parts (strings, bytes, `{ file: path }`) the
  host joins, so uploads send files the realm never reads.

Connector functions that produce or consume files take paths and require the
capability (`requireFiles`, `downloadTo`, `saveBytes` and `multipart` in
`_lib/files.ts`). They do not return or accept file contents as base64.

A function that always needs files carries `@requires files` in its JSDoc.
When `serve` runs without `--files-dir` (as in a Code Mode sandbox behind MCP),
its catalog leaves such functions out of search, `load`, the brief and
`invoke`, and drops a connector left with none. The CLI commands always show
the whole catalog; without `--files-dir` a tagged function fails when called. Functions that touch files only for some arguments
(an optional `path`, optional attachments) stay untagged. Loading warns about an
export whose body always calls `requireFiles`, `downloadTo` or `saveBytes` but
lacks the tag. Helpers reached through a local function are not seen, so the
tag stays the source of truth.

Exported functions take `(args, ctx: Ctx)`, need explicit parameter and return
types (TypeScript `isolatedDeclarations`), and a JSDoc comment, which becomes
the description in the brief. Return objects, not bare arrays.

Return upstream data as the API sends it. Unwrap envelopes and turn API errors
into thrown errors, but don't coerce or rename fields: types describe the
vendor's shape, using its field names, so drift shows up as real data rather
than silent nulls.
Type-check with `npx -p typescript tsc -p connectors`.

## HTTP API

All bodies are JSON. The HTTP API has no authentication: it is only safe
because a deployment's NetworkPolicy admits api-rs alone to the port, and which
principal a runner serves follows from where it runs rather than from a
request. Do not expose the port more widely.

| Route | Body / query | Result |
|---|---|---|
| `GET /healthz` | | `{ok, version, catalog_revision, connectors}`. Cheap and unauthenticated, it is the sandbox pod's readiness probe. |
| `POST /search` | `{query, limit?, policy?}` | `{results: [{tool, function, signature, description}], catalog_revision}` |
| `POST /load` | `{tool, policy?}` | `{tool, description, brief}`, or `404 {"error": "unknown tool <name>"}` |
| `POST /exec` | `{code, policy?, tools?}` | `{ok, result?, error?, logs, calls: [{tool, ok, error?, ms, requests: [...], files?}], ms}` |
| `GET /catalog?tools=a,b` | | `{connectors, brief, catalog_revision}` |

A failed cell is `200` with `ok: false`; a malformed body is axum's `422`.
`/exec` runs at most 32 cells at a time (`MAX_CELLS`) and answers
`503 {"error": "busy", "retryable": true}` beyond that, without queueing.
The calling context is informational: connectors may route on it, but nothing
authorizes from it.

- **Policy**: `{"allowlist": [...] | null, "blocklist": [...]}` is the
  deployment's tool filter. Entries match connector names exactly (`google-docs`
  does not match `google_docs`); an invisible connector looks unknown. `/exec`
  also takes the CLI's `tools` allowlist, and `policy` wins.
- **Search**: BM25 (`k1` 1.2, `b` 0.75) over one document per function: its
  name, the connector's name and description, its JSDoc and signature. `limit`
  defaults to 8 (max 50), applied after the policy filter.
- **Reload**: every 10 s the connector roots are hashed; a change loads and
  swaps in a new catalog, a failed load keeps the old one, and running cells
  keep theirs.
- **Idle exit**: after 7 days without a request the process exits `0` once no
  cell runs. `/healthz` is the pod's readiness probe and never counts as use.
- **Logging**: `info` by default (`RUST_LOG` overrides): one line per request
  except `/healthz` with the route, timing and counts, and for `/exec` a
  `connectors` summary (`tool.function:ok|err:<ms>ms[:<status>]`). Code,
  arguments, results, cell logs, bodies, headers and URLs are never logged.

## Tracing

A cell with a parent trace context exports OpenTelemetry spans over OTLP/HTTP
(the standard `OTEL_EXPORTER_OTLP_*` variables; `OTEL_TRACES_EXPORTER=none`
turns it off) as service `centaur-codemode`:

- `codemode.cell`, with call counts and the cell's status;
- a `TOOL` span per call, `codemode.tool.<connector>.<function>`;
- a client span per upstream request or websocket, `<METHOD> <host>`, with
  the method, the URL without its query string, and the status.

`exec` parents the cell on the running turn: harness-server writes the turn's
`traceparent`, `thread_key` and `execution_id` to `~/.centaur/turn-trace.json`
(`CENTAUR_TURN_TRACE_FILE`) when a turn starts and removes it when the turn
ends, so cell spans sit beside the harness's spans for that execution. Without
the file, a `TRACEPARENT` environment variable is used. `serve` takes the
`traceparent` header api-rs sends with the MCP tool call. With no parent,
nothing is exported. `exec` waits up to 3 s (or `OTEL_EXPORTER_OTLP_TIMEOUT`)
for the export before it exits. Spans never carry code, arguments, results,
logs or query strings.

## Environment

Every setting a sandbox or local run configures has a CLI flag and an
environment variable.

| Variable | Flag | Default | Meaning |
|---|---|---|---|
| `CENTAUR_CODEMODE_LISTEN` | `--listen` | `127.0.0.1:8790` | `serve` address. |
| `CENTAUR_CODEMODE_CONNECTORS` | `--connectors` | `connectors` | Connector roots, colon-separated. |
| `CENTAUR_CODEMODE_CA_FILE` | `--ca-file` | none | Extra PEM roots to trust, such as the proxy CA. |
| `CENTAUR_SANDBOX_CONTEXT_FILE` | `--context-file` | `~/.centaur/sandbox.json` | Sandbox context file. |
| `CENTAUR_CONSOLE_URL` | | none | Console base URL; `serve` uses it for the calling context when there is no context file. |
| `CENTAUR_CODEMODE_FILES_DIR` | `--files-dir` | none | Turns on the files capability, rooted here. |
| `CENTAUR_TURN_TRACE_FILE` | | `~/.centaur/turn-trace.json` | Turn trace file `exec` parents its spans on. |

## Limits (hard-coded)

Cell 120 s (busy loops are interrupted), tool call 60 s, 64 MiB heap and 1 MiB
stack per realm (the cell and each running call), 1000 calls and 8 concurrent
calls per cell, 10 MiB result, 10 MiB per HTTP response, 100 MiB per file
(read, written, downloaded or sent as a request body). A request may ask for
up to 10 minutes with `timeout_ms`, which extends its call and cell (plus 10 s
for the call to finish).

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
