# Spaces schema

Phase 2 plans these deployment-neutral durable tables:

- `space_definitions` — product-level spaces and opaque Centaur references
- `audit_events` — append-only actions taken in a space
- `budgets` — space budget policy and period limits
- `knowledge_index_metadata` — future index ownership and lifecycle metadata

Rules:

- Store opaque Centaur identifiers only (session id, principal id, secret id).
- Do not create foreign keys into Centaur/Postgres tables owned by `api-rs` or
  the console.
- The JSON Schemas here are contract stubs, not database migrations.
- Migrations and concrete tables arrive in Phase 2.
