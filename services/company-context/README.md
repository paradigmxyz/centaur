# Company Context

Standalone company-context ingestion service. It indexes regular Google Docs and text-bearing PDFs from users' My Drive, shared folders, Shared Drives they are members of, and Shared Drive folders shared with them without membership, using durable Absurd tasks. Google Docs are exported as plain text through the Drive API before chunking. Each user corpus and each member Shared Drive is followed through its own Drive change feed and checkpoint. Shared Drive folders shared with non-members have no change feed, so they are walked recursively each cycle, and files no walk has reached for 24 hours are removed.

It also indexes Granola meeting notes (title, summary, owner, and attendees) through each user's Granola MCP OAuth credential. Each sync lists the account's meetings from its checkpoint onward, fetches their details ten meetings at a time, and republishes a note only when its content changes. Transcripts are not indexed; the Granola tool fetches them on demand. Notes that no live Granola credential still observes are removed.

It also discovers the public and private Slack channels (and, when enabled, direct messages) each user belongs to, through the same per-user Slack broker credentials the Rails Console Slack DM sync uses. Each discovery records the credential's Slack identity and its channel memberships; channels that no live Slack credential still observes are removed, along with their messages. The first discovery each cycle to list a conversation syncs its message history. A sync reads the conversation's configured history days (`COMPANY_CONTEXT_SLACK_HISTORY_DAYS`, overridden per conversation by `COMPANY_CONTEXT_SLACK_CHANNEL_HISTORY_DAYS`) whenever part of that span has not been synchronized yet, so raising a value backfills the older history; lowering one keeps messages already synchronized. Otherwise it reads from its previous sync, always rereading the last 72 hours so edits within that window are picked up. Threads are synced whenever a parent message read this way shows a new reply; replies to threads started more than 72 hours earlier are not picked up. Only one sync reads a conversation at a time. Messages are stored privately in the system schema. Each discovery cycle also lists the workspace users through the app's bot token (`SLACK_BOT_TOKEN`), so documents show names instead of user IDs; the bot token needs the `users:read` scope and belongs to the same Slack app, so it shares the same rate-limit schedule. Users are removed once no live credential in their workspace remains. Slack tasks run on their own `company_context_slack` queue and worker.

After each sync, public and private channel history is projected into documents on the main queue. Each channel's UTC day is rendered as a transcript in which thread replies follow their parent, in the day the thread started. Joins, topic changes, and similar system messages are left out. The transcript is split into chunks of at most `COMPANY_CONTEXT_CHUNK_CHARS` characters that together cover the whole day: chunks break between messages, a thread split across chunks repeats the start of its parent, and only a single message longer than a chunk is split. Each chunk starts with its channel, date, and time range, and is published as its own document with its own embedding. A day is rendered again when its messages, its channel's name, or the name of a user it mentions changes, and republished only when its content changes; vectors are reused for chunks whose text is unchanged. Direct messages are not projected. Slack documents are not yet exposed to retrieval.

Slack limits each Web API method per workspace per app, and every token the app issues shares that budget, including bot tokens used by other services. Workers therefore reserve request slots from a shared schedule in `company_context_system.slack_rate_limits`, spaced so that ingestion uses only `COMPANY_CONTEXT_SLACK_RATE_LIMIT_SHARE` of each method's documented tier. When Slack rate limits a method, every worker waits out its `Retry-After` and the spacing widens, then relaxes while no rate limits occur. A task that must wait longer than a few seconds suspends instead of holding a worker.

The service owns these Postgres schemas:

- `company_context_system`: private cursors, staging (including Slack messages and users), and processing state.
- `company_context_data`: retrieval-facing Drive documents, Granola notes, and Slack channel documents, access observations (including Slack identities and channel memberships), and embeddings.

The `centaur_company_context_reader` role used by the company-context tool can read `google_drive_documents` and `google_drive_document_embeddings`. Row-level security limits each reader to files that a live broker credential with the same Google subject (`centaur.google_subject`) still observes; `google_drive_broker_observations` is the only source of that access. The reader cannot query the observations or the system schema directly. The reader has no access to Granola notes yet. The Helm deployment is gated by `experimentalCompanyContext.enabled` until it is ready for production.

## Required infrastructure

- Postgres with the existing Absurd schema and the `vector` and `pg_search` extensions available.
- `pdftotext` from Poppler (for PDF sources).
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
- `SLACK_BOT_TOKEN`: the Slack app's bot token, used to list workspace users (requires `users:read`)

The service discovers live per-user broker credentials belonging to the Google
OAuth app selected by `COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG` (default
`google`). Rails Console owns refreshing those tokens; the service decrypts the
current access token before each Drive request. Granola broker credentials are
selected the same way by `COMPANY_CONTEXT_GRANOLA_OAUTH_APP_SLUG` (default
`granola`), and Slack broker credentials by
`COMPANY_CONTEXT_SLACK_OAUTH_APP_SLUG` (default `slack`), which should match the
Console's Slack DM sync app. Drive requests honor `Retry-After`
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

Rollout limits (unset means no limit; values are comma-separated):

- `COMPANY_CONTEXT_GOOGLE_DRIVE_USER_EMAILS`: sync only these Google credential emails.
- `COMPANY_CONTEXT_GRANOLA_USER_EMAILS`: sync only these Granola credential emails.
- `COMPANY_CONTEXT_SLACK_USER_IDS`: sync only these Slack user IDs.
- `COMPANY_CONTEXT_SLACK_CHANNEL_IDS`: sync only these Slack conversation IDs.
- `COMPANY_CONTEXT_SLACK_CONVERSATION_TYPES` (default `public_channel,private_channel`): any of `public_channel`, `private_channel`, and `im`.

Emails match case-insensitively. Credentials outside a limit are treated like
dead credentials, so narrowing a limit removes data that only the excluded
users or conversations still observed.

Common optional settings:

- `IRON_CONTROL_DATABASE_NAME`
- `COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG` (default `google`)
- `COMPANY_CONTEXT_GRANOLA_OAUTH_APP_SLUG` (default `granola`)
- `COMPANY_CONTEXT_SLACK_OAUTH_APP_SLUG` (default `slack`)
- `BIND_ADDR` (default `0.0.0.0:8080`)
- `GOOGLE_DRIVE_API_BASE_URL`
- `GRANOLA_MCP_URL` (default `https://mcp.granola.ai/mcp`)
- `SLACK_API_BASE_URL` (default `https://slack.com/api`)
- `OPENAI_BASE_URL`
- `COMPANY_CONTEXT_SCAN_INTERVAL_SECONDS` (default `300`)
- `COMPANY_CONTEXT_GRANOLA_SYNC_INTERVAL_SECONDS` (default `1800`)
- `COMPANY_CONTEXT_GRANOLA_INITIAL_LOOKBACK_DAYS` (default `365`)
- `COMPANY_CONTEXT_SLACK_DISCOVERY_INTERVAL_SECONDS` (default `1800`)
- `COMPANY_CONTEXT_SLACK_HISTORY_DAYS` (default `90`, at most `36500`)
- `COMPANY_CONTEXT_SLACK_CHANNEL_HISTORY_DAYS`: comma-separated `CONVERSATION_ID=DAYS` overrides, e.g. `C0123456789=3650`
- `COMPANY_CONTEXT_SLACK_RATE_LIMIT_SHARE` (default `0.3`, greater than 0 and at most 1)
- `COMPANY_CONTEXT_SLACK_WORKER_CONCURRENCY` (default `4`)
- `COMPANY_CONTEXT_DRIVE_PAGE_SIZE` (default `100`)
- `COMPANY_CONTEXT_MAX_SCAN_PAGES` (default `10`)
- `COMPANY_CONTEXT_FOLDER_WALK_BATCH_SIZE` (default `50`, at most `100`)
- `COMPANY_CONTEXT_MAX_PDF_BYTES` (default `26214400`)
- `COMPANY_CONTEXT_MAX_EXTRACTED_BYTES` (default `52428800`; also limits exported Google Doc text)
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

## Development

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```
