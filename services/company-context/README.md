# Company Context

Standalone company-context ingestion service. The initial implementation indexes text-bearing PDF files from Google Drive using durable Absurd tasks.

The service owns these Postgres schemas:

- `company_context_system`: private cursors, staging, and processing state.
- `company_context_data`: retrieval-facing Drive documents, access observations, and embeddings.

The initial migrations deliberately add no retrieval-role grants or RLS policies. The corpus is populated for validation but is not exposed through the company-context tool yet.

## Required infrastructure

- Postgres with the existing Absurd schema and the `vector` and `pg_search` extensions available.
- `pdftotext` from Poppler.
- A Rails Console database containing a GCP auth credential with a control-plane service-account keyfile and an embeddings static credential.
- The Active Record encryption primary key and derivation salt used by Rails Console.

## Configuration

Every setting is available as both a command-line option and an environment
variable. Command-line options take precedence; run
`centaur-company-context --help` for the complete list.

Required:

- `DATABASE_URL`
- `IRON_CONTROL_DATABASE_URL`
- `IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY`
- `IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT`
- `COMPANY_CONTEXT_GOOGLE_CREDENTIAL_FOREIGN_ID`
- `COMPANY_CONTEXT_EMBEDDINGS_CREDENTIAL_FOREIGN_ID`

The Google foreign ID selects a Rails Console `gcp_auth_secrets` row backed by
a `control_plane` keyfile source. Its configured scopes and optional delegated
subject are used to mint short-lived Google access tokens. The embeddings
foreign ID selects a `static_secrets` row backed by a `control_plane` or
`token_broker` source. Encrypted values are loaded from Rails Console with the
shared Active Record encryption implementation.

Common optional settings:

- `IRON_CONTROL_DATABASE_NAME`
- `BIND_ADDR` (default `0.0.0.0:8080`)
- `GOOGLE_DRIVE_API_BASE_URL`
- `OPENAI_BASE_URL`
- `COMPANY_CONTEXT_SCAN_INTERVAL_SECONDS` (default `300`)
- `COMPANY_CONTEXT_DRIVE_PAGE_SIZE` (default `100`)
- `COMPANY_CONTEXT_MAX_SCAN_PAGES` (default `10`)
- `COMPANY_CONTEXT_MAX_PDF_BYTES` (default `26214400`)
- `COMPANY_CONTEXT_MAX_EXTRACTED_BYTES` (default `52428800`)
- `COMPANY_CONTEXT_EXTRACTION_TIMEOUT_SECONDS` (default `120`)
- `COMPANY_CONTEXT_CHUNK_CHARS` (default `6000`)
- `COMPANY_CONTEXT_CHUNK_OVERLAP_CHARS` (default `500`)
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
