# Company Context

Standalone company-context ingestion service. The initial implementation indexes text-bearing PDFs from users' My Drive, shared folders, Shared Drives they are members of, and Shared Drive folders shared with them without membership, using durable Absurd tasks. Each user corpus and each member Shared Drive is followed through its own Drive change feed and checkpoint. Shared Drive folders shared with non-members have no change feed, so they are walked recursively each cycle, and files no walk has reached for 24 hours are removed.

The service owns these Postgres schemas:

- `company_context_system`: private cursors, staging, and processing state.
- `company_context_data`: retrieval-facing Drive documents, access observations, and embeddings.

The initial migrations deliberately add no retrieval-role grants or RLS policies. The corpus is populated for validation but is not exposed through the company-context tool yet. The Helm deployment is gated by `experimentalCompanyContext.enabled` until it is ready for production.

## Required infrastructure

- Postgres with the existing Absurd schema and the `vector` and `pg_search` extensions available.
- `pdftotext` from Poppler.
- A Rails Console database containing live per-user Google OAuth broker credentials.
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

The service discovers live per-user broker credentials belonging to the Google
OAuth app selected by `COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG` (default
`google`). Rails Console owns refreshing those tokens; the service decrypts the
current access token before each Drive request. Drive requests honor `Retry-After`
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

Common optional settings:

- `IRON_CONTROL_DATABASE_NAME`
- `COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG` (default `google`)
- `BIND_ADDR` (default `0.0.0.0:8080`)
- `GOOGLE_DRIVE_API_BASE_URL`
- `OPENAI_BASE_URL`
- `COMPANY_CONTEXT_SCAN_INTERVAL_SECONDS` (default `300`)
- `COMPANY_CONTEXT_DRIVE_PAGE_SIZE` (default `100`)
- `COMPANY_CONTEXT_MAX_SCAN_PAGES` (default `10`)
- `COMPANY_CONTEXT_FOLDER_WALK_BATCH_SIZE` (default `50`, at most `100`)
- `COMPANY_CONTEXT_MAX_PDF_BYTES` (default `26214400`)
- `COMPANY_CONTEXT_MAX_EXTRACTED_BYTES` (default `52428800`)
- `COMPANY_CONTEXT_EXTRACTION_TIMEOUT_SECONDS` (default `120`)
- `COMPANY_CONTEXT_CHUNK_CHARS` (default `6000`)
- `COMPANY_CONTEXT_WORKER_CONCURRENCY` (default `4`)
- `COMPANY_CONTEXT_EMBEDDINGS_MODEL` (default `text-embedding-3-small`)
- `COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS` (currently required to be `1536`)

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
