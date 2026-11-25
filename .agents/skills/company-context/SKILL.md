---
name: company-context
description: "Use Centaur's indexed company context together with live Slack, Linear, Google Drive, Calendar, or Granola lookups, all through Code Mode connectors, when answering internal company-history, prior-decision, project-context, meeting-context, roadmap/status, or cross-source memory questions. Indexed context includes Slack channel history and files, Google Docs and PDFs, Google Calendar events, Granola notes, and custom documents visible to the requester. Use for questions like what was discussed, decided, planned, mentioned, or documented internally, especially when the user did not name one exact source."
---

# Company Context

Search indexed company memory first with the `company_context` connector, then check the live source connectors, and reconcile both before answering. Run everything in Code Mode (`centaur-codemode exec --result-only`); `centaur-codemode load company_context` prints the full contract.

`tools.company_context.search` returns only documents the requester can see: Slack channel-day transcripts and files, Google Docs and PDFs, Calendar events, Granola notes, and custom documents (archives, notes, memory). Each result has a `document_id`, `type`, `title`, `url`, `text`, `occurred_at`, and type-specific `metadata`.

## Default Workflow

1. Search company context first, before source-specific connectors. Default (hybrid) mode ranks by keyword and semantic similarity and is not paged; narrow it with filters when the question names a source, period, or channel:

```js
const cc = tools.company_context;
return await cc.search({
  query: "QUERY",
  limit: 10,                       // at most 50
  // types: ["slack_message", "slack_file", "drive_doc", "calendar_event", "granola_note", "custom_doc"],
  // occurred_after: "2025-01-01", occurred_before: "2025-02-01",
  // channel_ids: ["C123"],
});
```

To enumerate every match, such as all mentions of a term in a channel, set `keyword: true`: it returns every document containing all query terms, newest first and unranked, and a `next_cursor` to pass back as `cursor` with the same query and filters. Use keyword mode for exact identifiers, quoted phrases, and filenames.

```js
const all = [];
let cursor;
do {
  const page = await tools.company_context.search({
    query: "QUERY", keyword: true, channel_ids: ["C123"], occurred_after: "2025-01-01", limit: 50, cursor,
  });
  all.push(...page.results.map(({ document_id, title, url, occurred_at }) => ({ document_id, title, url, occurred_at })));
  cursor = page.next_cursor ?? undefined;
} while (cursor && all.length < 500);
return all;
```

For custom documents, filter with `sources` and `metadata` (an exact, case-sensitive subset match).

Query strategy:

- Keep the user's wording first and add at most one or two semantic variants; run them together with `Promise.all`.
- Add concrete domain anchors to ambiguous concepts and reject results matching only a broad neighbor.
- If fewer than two of the top five results directly answer the question, run one narrower variant.

2. Read the highest-value results in full before answering; do not infer conclusions from titles or snippets. Deduplicate repeated discussions:

```js
const ids = ["DOCUMENT_ID_1", "DOCUMENT_ID_2"];
return await Promise.all(ids.map((document_id) => tools.company_context.read_document({ document_id })));
```

3. Check live sources for the same query, even when indexed results look strong. One cell can query them all; use `Promise.allSettled` so one unavailable source does not hide the rest:

```js
const q = "QUERY";
const [linear, drive, calendar, granola] = await Promise.allSettled([
  tools.linear.search_issues({ query: q, limit: 10 }),
  tools.google_drive.list_files({ query: q, max_results: 10 }),
  tools.google_calendar.list_events({ query: q, max_results: 10 }),
  tools.granola.search_notes({ query: q, limit: 10 }),
]);
return { linear, drive, calendar, granola };
```

For Slack, check `tools.slack.get_access()` first. When its `search` is `company_context`, `slack.search_messages` queries the same index as step 1, so read live Slack with `slack.channel_history` or `slack.thread_replies` for the channels and threads the indexed results point to. When it is `slack`, `slack.search_messages` is Slack's native search and takes Slack search syntax.

Use the sources that match the question. For broad cross-source questions, check all likely sources.

4. For time-sensitive or "latest" asks, a keyword search returns newest first, so its first `occurred_at` shows the newest indexed match. Confirm current state with the live connectors, since the index can lag.

5. Collate indexed and live results before answering:

- Prefer live evidence for current state, exact source objects, and canonical fields.
- Prefer indexed context for historical recall, older discussions, and cross-source semantic matches.
- Treat disagreement as a signal to explain the mismatch, including dates and source links when available.
- Avoid claiming completeness unless the owning source supports that claim.

## When To Go Deeper

Use extra live-connector depth after the default pass when:

- The user asks for the current Slack thread, a specific Slack channel, message, or file, or a source object that company context did not return.
- The user asks to create, update, comment on, or inspect an exact Linear issue or project.
- The user gives an exact Google Doc, Drive, Calendar, or Linear URL or ID (`google_docs.get_text`, `google_drive.get_file`, `linear.issue`).
- `company_context.search` returns an auth, permission, connection, or malformed-response error. Report it; do not treat it as an empty result.
- The question requires a canonical exhaustive answer. Company context is indexed retrieval; use the owning API for definitive inventories or yes/no internal-history claims when the user asks for completeness.

## Answering Rules

- Distinguish direct internal views from AI-generated research or summaries; prefer human-authored discussion and primary notes.
- If retrieval remains weak, say so rather than synthesizing a confident company position.
- Lead with the answer and cite the underlying thread or document when it matters: source, type, date, title, and URL or permalink if present.
- Say how indexed context and live results agree, differ, or complement each other.
- Do not copy long private documents or message dumps into the reply. Summarize narrowly and quote only short snippets when useful.
- If no indexed context is found after reasonable query variants, say that plainly and name the live sources checked.
