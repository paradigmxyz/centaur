# Spaces schema (placeholder)

Future durable product state for Spaces lives here: space definitions, knowledge
index metadata, audit ledger, and budgets.

Rules:

- Store opaque Centaur identifiers only (session id, principal id, secret id).
- Do not create foreign keys into Centaur/Postgres tables owned by `api-rs` or
  the console.
- Migrations and concrete tables arrive in Phase 2.
