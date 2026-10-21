# Company Context

Standalone company-context ingestion service. It indexes regular Google Docs and text-bearing PDFs from users' My Drive, shared folders, Shared Drives they are members of, and Shared Drive folders shared with them without membership, using durable Absurd tasks. Google Docs are exported as plain text through the Drive API before chunking. Each user corpus and each member Shared Drive is followed through its own Drive change feed and checkpoint. Shared Drive folders shared with non-members have no change feed, so they are walked recursively each cycle, and files no walk has reached for 24 hours are removed.

It also indexes Google Calendar events through the same Google broker credentials, when they include the `calendar.readonly` scope. Each sync cycle lists the calendars each credential can read event details of (reader, writer, or owner access) and syncs each one through that credential with Google's incremental sync tokens; a stored token Google expires or rejects starts over with a full sync, which removes the events it no longer lists. Events are stored per credential, as Google showed them to it, so private events on calendars the user can only read stay redacted by Google; a calendar shared by several users is stored once per user. Only events that ended within the last `COMPANY_CONTEXT_CALENDAR_HISTORY_DAYS`, and upcoming events, are indexed; each sync removes events that have since aged out. Raising the value starts a full sync that backfills the older events; lowering it removes the events that fall outside. Recurring events are indexed once, by their first occurrence, and kept however long ago their series started. Each event (title, times, recurrence, location, organizer, attendees, and description, converted from HTML to plain text) is published as its own document as it is synced. Calendar events are searched by keyword only; they are not embedded. A calendar its credential no longer lists, or whose credential is dead or deleted, is removed with its events.

It also indexes Granola meeting notes (title, summary, owner, and attendees) through each user's Granola MCP OAuth credential. Each sync lists the account's meetings from its checkpoint onward, fetches their details ten meetings at a time, and republishes a note only when its content changes. Transcripts are not indexed; the Granola tool fetches them on demand. Notes that no live Granola credential still observes are removed.

It also discovers the public and private Slack channels (and, when enabled, direct messages) each user belongs to, through the same per-user Slack broker credentials the Rails Console Slack DM sync uses. The app's bot token (`SLACK_BOT_TOKEN`) is synced as one more credential: it discovers the public and private channels the bot is a member of and can sync their history and files, but never its direct messages. The bot is not a principal, so channels only it observes are indexed without being visible to anyone until a user credential observes them too. While `COMPANY_CONTEXT_SLACK_USER_IDS` is set, the bot is synced only if the limit lists the bot's Slack user ID. Each discovery records the credential's Slack identity and its channel memberships; channels that no live Slack credential still observes are removed, along with their messages. The first discovery each cycle to list a conversation syncs its message history. A sync reads the conversation's configured history days (`COMPANY_CONTEXT_SLACK_HISTORY_DAYS`, overridden per conversation by `COMPANY_CONTEXT_SLACK_CHANNEL_HISTORY_DAYS`) whenever part of that span has not been synchronized yet, so raising a value backfills the older history; lowering one keeps messages already synchronized. Otherwise it reads from its previous sync, always rereading the last 72 hours so edits within that window are picked up. Threads are synced whenever a parent message read this way shows a new reply; replies to threads started more than 72 hours earlier are not picked up. Only one sync reads a conversation at a time. Messages are stored privately in the system schema. Each discovery cycle also lists the workspace users through the app's bot token (`SLACK_BOT_TOKEN`), so documents show names instead of user IDs; the bot token needs the `users:read` scope and belongs to the same Slack app, so it shares the same rate-limit schedule. Users are removed once no live credential in their workspace remains. Slack tasks run on their own `company_context_slack` queue and worker, except thread syncs, which run in order on the `company_context_slack_threads` queue so a large backlog of threads does not delay discovery and history syncs.

It also projects documents operators import directly into the database (see
[Imported documents](#imported-documents)). Imported documents are public:
every principal can read them.

After each sync, the conversation's history is projected into documents on the main queue. Each channel's UTC day is rendered as a transcript in which thread replies follow their parent, in the day the thread started. Joins, topic changes, and similar system messages are left out. The transcript is split into chunks of at most `COMPANY_CONTEXT_CHUNK_CHARS` characters that together cover the whole day: chunks break between messages, a thread split across chunks repeats the start of its parent, and only a single message longer than a chunk is split. Each chunk starts with its channel, date, and time range, and is published as its own document with its own embedding. A day is rendered again when its messages, its channel's name, or the name of a user it mentions changes, and republished only when its content changes; vectors are reused for chunks whose text is unchanged. Slack documents are exposed to retrieval only through the query endpoints.

Files attached to projected messages are indexed as their own documents, not in the channel day transcripts, which only name them. Projection records which messages share each file, so a file shared in several channels is extracted once. Each file is downloaded with a live credential observing one of its conversations whose scopes include `files:read`; without one, files wait until such a credential syncs. A credential Slack refuses a file to is not used for that file again until the file changes. Files whose extraction or publication has not finished within an hour are enqueued again by their conversation's next projection. PDFs are extracted with `pdftotext`; Word, PowerPoint, and Excel files (`docx`, `pptx`, `xlsx`), OpenDocument text, RTF, and EPUB with sandboxed `pandoc`; and snippets and other text files directly. Legacy binary Office formats, images, canvases, and external files such as Google Docs links are not indexed. Downloaded bytes are discarded after extraction, and only the extracted text, chunked like Drive documents, is stored. A file is extracted again when Slack reports different content for it, and keeps its published text until the new version replaces it; a version that cannot be indexed removes it. Its documents are also removed when Slack reports it deleted, and the file is removed once no stored message shares it.

Slack limits each Web API method per workspace per app, and every token the app issues shares that budget, including bot tokens used by other services. Workers therefore reserve request slots from a shared schedule in `company_context_system.slack_rate_limits`, spaced so that ingestion uses only `COMPANY_CONTEXT_SLACK_RATE_LIMIT_SHARE` of each method's documented tier. A task waits for its slot in place, holding its worker, so only running calls hold slots and queued tasks wait their turn on the queue. When Slack rate limits a method, every worker waits out its `Retry-After`, and the rate-limited call retries before tasks queued behind it.

The service owns these Postgres schemas:

- `company_context_system`: private cursors, staging (including Slack messages and users, and imported documents), and processing state.
- `company_context_data`: retrieval-facing Drive documents, Calendar events, Granola notes, Slack channel and file documents, imported document chunks, access observations (including Slack identities, channel memberships, and the conversations each Slack file is shared in), and embeddings. The query endpoints read only this schema, as the `centaur_company_context_v2_query` role, which can only select from it; the service switches to that role for each query transaction, so its login role must be able to grant itself membership (`CREATEROLE` or superuser).

Documents are exposed to retrieval only through the query endpoints; no other database role can read them. The Helm deployment is gated by `experimentalCompanyContext.enabled` until it is ready for production.

## Required infrastructure

- Postgres with the existing Absurd schema and the `vector` and `pg_search` extensions available.
- `pdftotext` from Poppler (for PDF sources).
- `pandoc` 3.8.3 or later (for Office, OpenDocument, RTF, and EPUB Slack files).
- A Rails Console database containing live per-user Google and Granola OAuth broker credentials.
- The Active Record encryption primary key and derivation salt used by Rails Console.
- An embeddings API key supplied through a Kubernetes Secret.

## Configuration

Every setting is available as both a command-line option and an environment
variable. Command-line options take precedence; run
`centaur-company-context --help` for the complete list.

Required:

- `DATABASE_URL`
- `IRON_CONTROL_DATABASE_URL`
- `IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY`
- `IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT`
- `OPENAI_API_KEY`
- `SLACK_BOT_TOKEN`: the Slack app's bot token, used to list workspace users (requires `users:read`) and synced as a Slack credential (with `channels:read`, `channels:history`, `groups:read`, `groups:history`, and `files:read` for the channels it is in)
- `CENTAUR_JWT_SIGNING_SECRET`: the secret the Console signs principal API JWTs with, used to authenticate the query endpoints

The service discovers live per-user broker credentials belonging to the Google
OAuth app selected by `COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG` (default
`google`); Drive sync uses those with the `drive.readonly` scope and Calendar
sync those with the `calendar.readonly` scope. Rails Console owns refreshing
those tokens; the service decrypts the current access token before each Google
request. Granola broker credentials are
selected the same way by `COMPANY_CONTEXT_GRANOLA_OAUTH_APP_SLUG` (default
`granola`), and Slack broker credentials by
`COMPANY_CONTEXT_SLACK_OAUTH_APP_SLUG` (default `slack`), which should match the
Console's Slack DM sync app. Google requests honor `Retry-After`
on rate limits and retry server errors with bounded exponential backoff. Durable
document tasks record known permanent content and request failures as `rejected`
instead of retrying them. A credential reconciliation task deactivates
observations from dead or deleted broker credentials and removes files only when
no live user credential can still observe them. Each scan interval also lists
every credential's Shared Drives and the Shared Drive items shared with it,
enqueues a scan per member drive, starts a folder walk per other drive, and revokes
that credential's access to files in drives it can no longer reach. A folder
walk runs as one Absurd task per batch of folders: each batch lists its
folders' children in a single Drive search and spawns batches for the
subfolders. The Helm deployment reads
`OPENAI_API_KEY` directly from the shared Kubernetes Secret.

Indexer switches (default `true`):

- `COMPANY_CONTEXT_DRIVE_ENABLED`: schedule Google Drive sync.
- `COMPANY_CONTEXT_CALENDAR_ENABLED`: schedule Google Calendar sync.
- `COMPANY_CONTEXT_GRANOLA_ENABLED`: schedule Granola sync.
- `COMPANY_CONTEXT_SLACK_ENABLED`: schedule Slack sync and run the Slack queue worker.
- `COMPANY_CONTEXT_IMPORTED_ENABLED`: schedule projection of imported documents.

Disabling an indexer pauses it without removing anything: no new sync or
reconciliation is scheduled, already indexed documents stay searchable, and
enabling it again resumes from the stored checkpoints. Tasks already queued on
the main queue still finish; queued Slack tasks wait until Slack is enabled
again. Every required setting, including `SLACK_BOT_TOKEN`, is still required.

Rollout limits (unset means no limit; values are comma-separated):

- `COMPANY_CONTEXT_GOOGLE_DRIVE_USER_EMAILS`: sync only these Google credential emails.
- `COMPANY_CONTEXT_GOOGLE_CALENDAR_USER_EMAILS`: sync calendars only through these Google credential emails.
- `COMPANY_CONTEXT_GRANOLA_USER_EMAILS`: sync only these Granola credential emails.
- `COMPANY_CONTEXT_SLACK_USER_IDS`: sync only these Slack user IDs; include the bot's user ID to keep syncing the bot token.
- `COMPANY_CONTEXT_SLACK_CHANNEL_IDS`: sync only these Slack conversation IDs.
- `COMPANY_CONTEXT_SLACK_CONVERSATION_TYPES` (default `public_channel,private_channel`): any of `public_channel`, `private_channel`, and `im`.

Emails match case-insensitively. Credentials outside a limit are treated like
dead credentials, so narrowing a limit removes data that only the excluded
users or conversations still observed.

Common optional settings:

- `IRON_CONTROL_DATABASE_NAME`
- `CENTAUR_API_JWT_AUDIENCE` (default `centaur-api`) and `CENTAUR_API_JWT_ISSUER` (default `centaur-console`): must match the Console's API JWT settings
- `COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG` (default `google`)
- `COMPANY_CONTEXT_GRANOLA_OAUTH_APP_SLUG` (default `granola`)
- `COMPANY_CONTEXT_SLACK_OAUTH_APP_SLUG` (default `slack`)
- `BIND_ADDR` (default `0.0.0.0:8080`)
- `GOOGLE_DRIVE_API_BASE_URL`
- `GOOGLE_CALENDAR_API_BASE_URL` (default `https://www.googleapis.com/calendar/v3`)
- `GRANOLA_MCP_URL` (default `https://mcp.granola.ai/mcp`)
- `SLACK_API_BASE_URL` (default `https://slack.com/api`)
- `SLACK_FILES_BASE_URL` (default `https://files.slack.com`): the only origin Slack file downloads, which carry user tokens, are sent to
- `OPENAI_BASE_URL`
- `COMPANY_CONTEXT_SCAN_INTERVAL_SECONDS` (default `300`)
- `COMPANY_CONTEXT_CALENDAR_SYNC_INTERVAL_SECONDS` (default `1800`)
- `COMPANY_CONTEXT_CALENDAR_HISTORY_DAYS` (default `90`, at most `36500`)
- `COMPANY_CONTEXT_GRANOLA_SYNC_INTERVAL_SECONDS` (default `1800`)
- `COMPANY_CONTEXT_GRANOLA_INITIAL_LOOKBACK_DAYS` (default `365`)
- `COMPANY_CONTEXT_IMPORTED_PROJECT_INTERVAL_SECONDS` (default `60`)
- `COMPANY_CONTEXT_SLACK_DISCOVERY_INTERVAL_SECONDS` (default `1800`)
- `COMPANY_CONTEXT_SLACK_HISTORY_DAYS` (default `90`, at most `36500`)
- `COMPANY_CONTEXT_SLACK_CHANNEL_HISTORY_DAYS`: comma-separated `CONVERSATION_ID=DAYS` overrides, e.g. `C0123456789=3650`
- `COMPANY_CONTEXT_SLACK_RATE_LIMIT_SHARE` (default `0.3`, greater than 0 and at most 1)
- `COMPANY_CONTEXT_SLACK_WORKER_CONCURRENCY` (default `4`)
- `COMPANY_CONTEXT_SLACK_THREAD_WORKER_CONCURRENCY` (default `1`)
- `COMPANY_CONTEXT_DRIVE_PAGE_SIZE` (default `100`)
- `COMPANY_CONTEXT_MAX_SCAN_PAGES` (default `10`)
- `COMPANY_CONTEXT_FOLDER_WALK_BATCH_SIZE` (default `50`, at most `100`)
- `COMPANY_CONTEXT_MAX_PDF_BYTES` (default `26214400`; also limits Slack file downloads)
- `COMPANY_CONTEXT_MAX_EXTRACTED_BYTES` (default `52428800`; also limits exported Google Doc text and extracted Slack file text)
- `COMPANY_CONTEXT_EXTRACTION_TIMEOUT_SECONDS` (default `120`)
- `COMPANY_CONTEXT_CHUNK_CHARS` (default `6000`)
- `COMPANY_CONTEXT_WORKER_CONCURRENCY` (default `4`)
- `COMPANY_CONTEXT_EMBEDDINGS_MODEL` (default `text-embedding-3-small`)
- `COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS` (currently required to be `1536`)

## Backfilling newly supported Drive file types

To discover existing files after adding a supported Drive MIME type, stop the
company-context workers and run:

```bash
psql "$DATABASE_URL" --file scripts/reset_drive_checkpoints.sql
```

Restart the workers afterward. This resets only Drive scan cursors. The fresh
metadata scan does not enqueue extraction for unchanged files, so previously
indexed PDFs are not downloaded again.

## Endpoints

- `GET /healthz`
- `GET /readyz`
- `GET /metrics`
- `POST /query`
- `GET /documents/{document_id}`

Both query endpoints are authenticated with the same principal API JWT the
Console mints for api-rs (`Authorization: Bearer <jwt>`); iron-proxy injects it
into sandbox requests to this service. The token's subject names the principal,
whose Slack user ID and granted broker credentials are looked up in the Rails
Console database. The principal's Google and Granola identities are the
subjects of the live Google and Granola broker credentials (from
`COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG` and
`COMPANY_CONTEXT_GRANOLA_OAUTH_APP_SLUG`) granted directly to it; role grants
and principal labels do not count. A document is visible only while an active
broker observation for one of those identities still reaches its Drive file,
Slack conversation (for Slack files, any conversation the file is shared in),
or Granola note; a Calendar event only to the Google identity it was synced
for. Imported documents are visible to every principal. A principal without one of those identities sees no documents
of the corresponding types. Disabling an OAuth app in the Console removes the
corresponding identity, so its documents stop being returned without being
removed from the index.

Granola does not report notes that are unshared or deleted, so a note stays
visible to the principals whose credentials once observed it until those
credentials are dead or deleted.

Errors return `{"error": "..."}` with status 400 for an invalid request, 401
for a missing or invalid token, 403 for a principal unknown to the Console, and
404 for a document that does not exist or is not visible.

### `POST /query`

Searches the visible Slack channel documents, Slack file documents, Drive
documents, Calendar events, Granola notes, and imported documents.

```json
{
  "query": "falcon launch plan",
  "filters": {
    "types": ["slack_message", "slack_file", "drive_doc", "calendar_event", "granola_note", "imported_doc"],
    "occurred_after": "2024-01-01T00:00:00Z",
    "occurred_before": "2024-02-01T00:00:00Z",
    "channel_ids": ["C0123456789"],
    "file_ids": ["F0123456789"],
    "sources": ["archive"],
    "metadata": { "author": "Ada" }
  },
  "limit": 10
}
```

`filters`, each filter, and `limit` are optional; unknown fields are rejected.
Filters combine with AND:

- `types`: the data types to search. Absent or empty searches every type.
- `occurred_after` (inclusive) and `occurred_before` (exclusive): RFC 3339
  timestamps. A Slack channel document matches when its messages overlap the
  window; a Slack file matches by when it was created, a Drive document by
  when it was last modified, a Calendar event when it overlaps the window, its
  end being exclusive (a recurring event by its first occurrence), a
  Granola note by when its meeting occurred, and an imported document by its
  `occurred_at`.
  Documents without a timestamp do not match a window.
- `channel_ids`: Slack conversation IDs, at most 100. Searches only Slack
  messages in these conversations and Slack files shared in them; a file
  matches only through a conversation the principal can see.
- `file_ids`: Slack or Drive file IDs, at most 100. Searches only these files'
  documents.
- `sources`: import sources, at most 100. Searches only documents imported to
  them.
- `metadata`: a JSON object of at most 65,536 bytes. Searches only imported
  documents whose returned `metadata`, including `source` and `external_id`,
  contains it, as Postgres `@>` defines: values match exactly, nested objects
  match recursively, and an array matches when it contains every listed
  element. An empty object is ignored.

`channel_ids`, `file_ids`, `sources`, and `metadata` restrict the search to the types they apply to; a
request whose types cannot satisfy every filter is rejected. `limit` defaults
to 10 and is at most 50; there is no pagination, so narrow the filters instead.
Keyword (BM25) and embedding similarity ranks are combined with reciprocal rank
fusion; if the query cannot be embedded, keyword ranks alone are used.
Calendar events have only a keyword rank, so they tend to rank below other
types' documents that match both ways. A result's `score` is its fused rank
score, comparable only within one response.

```json
{
  "results": [
    {
      "document_id": "slack:C0123456789:2024-01-02:000000",
      "type": "slack_message",
      "title": "#general — 2024-01-02",
      "url": null,
      "text": "#general · 2024-01-02 · 09:00–09:30 UTC\n\n[09:00] Ada: ...",
      "occurred_at": "2024-01-02T09:00:00Z",
      "score": 0.0325,
      "metadata": { "conversation_id": "C0123456789", "channel_name": "general" }
    }
  ]
}
```

`text` is the full document chunk. `metadata` holds type-specific fields:
conversation, channel, and message times for `slack_message`; file ID and file
type for `slack_file`; file ID, document type, MIME type, and pages for
`drive_doc`; calendar ID, event ID, start and end times, and organizer for
`calendar_event`; note ID, owner, and attendees for `granola_note`; the
imported metadata with the source and external ID for `imported_doc`.

### `GET /documents/{document_id}`

Returns one visible document, by a `document_id` from `POST /query`, in the
same shape as a query result without `score`.

## Imported documents

Operators import public documents by writing rows to
`company_context_system.imported_documents` with any role that can write to
the table, for example with `INSERT ... ON CONFLICT` or `\copy`:

| Column | |
|---|---|
| `source` | Required. Names the collection: 1-64 lowercase letters, digits, or hyphens, starting with a letter or digit. |
| `external_id` | Required. The document's ID within the source, 1-512 bytes. `(source, external_id)` is the primary key. |
| `title` | At most 1,000 characters. |
| `body` | Required, not blank, at most 1,000,000 bytes. |
| `occurred_at` | When the document's content occurred; used by time filters. |
| `metadata` | A JSON object of at most 65,536 bytes, such as a link or author. Returned with the document and matched by the `metadata` query filter, but not searched by text; `source` and `external_id` keys are overridden. |

Check constraints reject invalid rows on write. A trigger maintains
`content_hash`, `created_at`, and `updated_at`.

Every `COMPANY_CONTEXT_IMPORTED_PROJECT_INTERVAL_SECONDS`, the service chunks
and embeds new and changed rows, title and body together, and publishes them as
`imported_doc` documents. A run projects at most 1,000 rows, so a large import
is projected over several runs. Rows the embeddings API rejects are recorded in
`company_context_system.imported_document_rejections` and retried only once
they change. Deleting a row removes its documents immediately. To change a
projected row's `source` or `external_id`, delete it and insert it again.

## Development

The service's code lives in `crates/centaur-company-context`. From the
repository root:

```bash
cargo fmt -p centaur-company-context --check
cargo clippy --locked -p centaur-company-context --all-targets -- -D warnings
cargo test --locked -p centaur-company-context
```
