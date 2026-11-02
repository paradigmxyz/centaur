# Websearch Plugin

Web search through one vendor per call, chosen with `--backend` (Python:
`backend=`). The default comes from the `WEBSEARCH_BACKEND` sandbox environment
variable (for example through `SESSION_SANDBOX_EXTRA_ENV`), then `parallel`:

| Backend              | Credential                                                                  |
| -------------------- | --------------------------------------------------------------------------- |
| `parallel` (default) | Optional `PARALLEL_API_KEY`. When the key is rejected, search uses the free anonymous Parallel Search MCP. |
| `exa`                | `EXA_API_KEY`                                                               |
| `tako`               | `TAKO_API_KEY`                                                              |

Free Parallel results include `meta.attribution`; keep it when redistributing
them (<https://parallel.ai/customer-terms>).

## CLI

```bash
websearch search "Recent funding for AI search startups" \
  -n 5 --include-domain techcrunch.com --max-age-hours 720 --pretty
```

| Option | Meaning |
| --- | --- |
| `--num-results`, `-n` | Maximum results (default 10) |
| `--include-domain`, `--exclude-domain` | Domain filters. Repeatable. |
| `--max-age-hours` | Recency filter. Parallel and Tako round it down to a UTC date. |
| `--mode` | `basic` (faster) or `advanced` |
| `--max-chars-total` | Upper bound on total snippet characters |
| `--session-id` | Stable ID reused across related searches. Parallel only. |
| `--timeout-seconds` | Request timeout (default 60) |
| `--backend` | `parallel`, `exa`, or `tako` |
| `--pretty` | Print titles and URLs instead of JSON |

The free Parallel path ignores the domain, recency, and mode filters. Any
ignored option is listed in `meta.warnings`.

## Output

```json
{
  "query": "...",
  "results": [{"title": "...", "url": "...", "snippet": "...", "published_date": "2026-01-02"}],
  "meta": {"backend": "parallel:api", "request_id": "...", "warnings": []}
}
```

`meta.backend` is `parallel:api`, `parallel:free`, `exa`, or `tako`.
