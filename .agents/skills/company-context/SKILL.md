---
name: company-context
description: "Use Centaur's indexed company context together with direct Slack, Linear, Google Docs, Drive, or Calendar searches when answering internal company-history, prior-decision, project-context, meeting-context, roadmap/status, or cross-source memory questions. Indexed context includes Slack channels, user-visible Slack DMs, Google Docs, Google Calendar, and Linear. Use for questions like what was discussed, decided, planned, mentioned, or documented internally, especially when the user did not name one exact source."
---

# Company Context

Use `company_context` as the first retrieval step for internal historical context. Its `search` command queries indexed company memory across enabled sources such as Slack channels, Google Docs (`--source docs`), Google Calendar, and Linear. It also has dedicated commands for user-visible Slack DMs and DM conversations. Always pair indexed results with the relevant direct source tools, then reconcile and collate both evidence sets before answering.

## Default Workflow

1. Search company context first:

```bash
company_context search "QUERY" --limit 10 --json
```

For DM-specific questions, use the dedicated DM search surface:

```bash
company_context search-dms "QUERY" --limit 10 --json
company_context search-dm-conversations "PERSON OR QUERY" --limit 10 --json
```

2. Read promising documents before answering:

```bash
company_context read "DOCUMENT_ID" --max-chars 8000 --related --json
```

3. Check live/source tools for the same query and relevant source filters. Do this even when indexed results look strong:

```bash
slack search "QUERY"
linear search "QUERY"
gsuite drive list --query "QUERY"
gsuite calendar events --query "QUERY"
```

Use the source-specific tools that match the question. For broad cross-source questions, check all likely sources.

4. For time-sensitive or "latest" asks, check index freshness:

```bash
company_context latest-date
company_context latest-date --source slack
company_context latest-date --source docs --source-type google_doc
company_context latest-date --source linear
```

`latest-date` always outputs JSON, so it does not accept a `--json` flag.

5. If results are weak, broaden or target source filters:

```bash
company_context search "QUERY" --source slack --limit 10 --json
company_context search "QUERY" --source docs --source-type google_doc --limit 10 --json
company_context search "QUERY" --source google_calendar --limit 10 --json
company_context search "QUERY" --source linear --limit 10 --json
company_context search-dms "QUERY" --limit 10 --json
```

6. Collate indexed and live results before answering:

- Prefer direct live-tool evidence for current state, exact source objects, and canonical fields.
- Prefer indexed context for historical recall, older discussions, and cross-source semantic matches.
- Treat disagreement as a signal to explain the mismatch, including dates and source links when available.
- Avoid claiming completeness unless the direct source tool or owning API supports that claim.

## When To Fall Back

Use extra direct-tool depth after the default indexed-plus-live pass when:

- The user asks for the current Slack thread, a specific Slack channel/message/file, or a direct source object that `company_context` did not return.
- The user asks to create, update, comment on, or inspect an exact Linear issue/project.
- The user gives an exact Google Doc, Drive, Calendar, or Linear URL/id.
- `company_context` returns an auth, permission, connection, or malformed-response error.
- The question requires a canonical exhaustive answer. Company context is indexed retrieval; use the owning API/database for definitive inventories or yes/no internal-history claims when the user asks for completeness.

## Answering Rules

- Lead with the answer and include the evidence source when it matters: source, source type, date, title, and URL/permalink if present.
- Say how indexed context and live source-tool results agree, differ, or complement each other.
- If freshness matters, use `latest-date` to inspect the indexed cutoff and direct source tools for live corroboration.
- Do not copy long private documents or message dumps into the reply. Summarize narrowly and quote only short snippets when useful.
- If no indexed context is found after reasonable query variants, say that plainly and name the live tools checked.
