# tako

Structured, cited data from [Tako](https://tako.com): knowledge cards backed by
licensed sources, synthesized answers, and free data-coverage discovery.

Where `websearch` retrieves prose from web pages, Tako returns typed series and
comparable metrics with sources attached. The two complement each other. Use
`websearch` for narrative and recency, and reach for `tako` when the answer is
a number, a trend, or a cohort comparison.

## Setup

```bash
TAKO_API_KEY=...   # https://tako.com, account settings
```

## The flow: discover free, then search priced

```bash
# 1. Free: does Tako have data on this, and what is it called?
tako available-data "Tesla"

# 2. Priced: pull the card, reusing the exact name (and optionally the node id)
tako search "Tesla, Inc. Revenue" --node-id <node_id from step 1>

# 3. Priced beyond the free allowance: export the full dataset behind a card
tako contents "<card webpage_url>" --content-format json_records
```

`available-data` is the recommended first step for any data lookup. It costs
nothing, it confirms coverage before you spend a priced call, and the exact
metric and entity names it returns let the follow-up land precisely instead of
guessing. Skip it only for an obvious open-web query that no data graph would
hold.

It is type-aware. An **entity** ("Tesla") reports the metrics Tako tracks for
it; a **metric** ("Inflation Rate") reports the entities it is tracked across.
One metric across many entities is a single metric-first call, and one entity
across many metrics is a single entity-first call. Never loop one call per
name.

In the output, `found` is true only when a match has live coverage; node
resolution alone never counts. `coverage.names` lists the exact names to reuse
verbatim, and `total`/`truncated`/`capped` report when more exist server-side.
`capped` means the server stopped counting, so read the total as "at least N".

## Commands

```bash
tako health
tako available-data "inflation rate" --types metric
tako search "Nvidia vs AMD revenue" --effort deep
tako answer "How has US CPI moved since 2020?"
tako contents "<card webpage_url>" --mode inline --content-format csv
```

`search` and `answer` take the same options. `--data-count 0` gives a web-only
search and `--web-count 0` gives a data-only search, since a source is queried
only when its key is present in the request. `--node-id` (repeatable) pins
graph nodes from `available-data` as retrieval candidates, which is a strong
boost; add `--strict` to return only cards matching the pinned nodes.

Node ids are **not durable** across knowledge-graph rebuilds. Resolve them in
the same session you use them; don't cache them across days.

## Cost

`available-data` and `health` are free. `search` and `answer` are billed per
call, and `--effort deep` costs more than the `fast` default. `contents`
includes a 20-row free allowance per card export and bills per 1,000 rows
beyond it. Pass `--quote-only` to price an export without paying for it.

## Notes for maintainers

`available-data` is a port of the `tako_available_data` tool from the
[tako-mcp](https://github.com/TakoData/tako-mcp) workers: one `graph/search`
(limit 10), then a type-aware `graph/related` drill on the top 2 hits with
per-node error isolation, then a deterministic summary. No LLM call is
involved. The pure selection and formatting logic lives in `_coverage.py`
with unit tests in `tests/test_client.py`, and `client._run_available_data`
orchestrates the fetches. The raw graph primitives remain as private
`TakoClient._graph_search`/`_graph_related` methods, so re-exposing pagination
or cohort walking later is a CLI change rather than a rebuild.

Tests live in `tests/` (no `__init__.py`) rather than at the tool root. The
tool directory shares the SDK's import name, and pytest's module naming would
otherwise load this package as top-level `tako` and shadow the SDK.

Three SDK workarounds worth knowing about, all in `client.py`:

- **Graph auth.** The SDK's OpenAPI spec is missing security declarations on
  the beta graph endpoints, so the generated client attaches no `X-API-Key`
  to `graph_search`/`graph_related` and the API answers 401. The client sets
  the key as a default header on the underlying `ApiClient`, which covers
  every operation. Remove that line once the spec is fixed upstream.

- **Proxy wiring.** `tako-sdk`'s transport is urllib3, which does not read
  proxy settings from the environment. With `Configuration.proxy` unset it
  builds a plain `PoolManager` and dials the upstream directly. The request
  then bypasses iron-proxy entirely: the credential placeholder is never
  swapped, and the sandbox NetworkPolicy drops the connection. The client
  reads `HTTPS_PROXY` and `SSL_CERT_FILE`/`REQUESTS_CA_BUNDLE` explicitly,
  the same shape as `tools/productivity/gsuite/client.py`.
- **Timeouts.** The SDK accepts `_request_timeout` per request, but the `Tako`
  facade doesn't forward it, so there is no per-call timeout available through
  the public surface. `Configuration(retries=...)` is wired instead. Worth an
  upstream request.

The SDK agent products (`client.agent.retrieval`, `client.agent.answer`) are
deliberately not exposed. They are async run/poll/stream, which fits a
workflow better than a one-shot CLI shim.
